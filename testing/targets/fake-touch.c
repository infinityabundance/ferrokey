/*
 * fake-touch — a virtual touchscreen for the Ferrokey compatibility courts.
 *
 * Creates a real uinput touchscreen device inside the guest VM and injects
 * touch taps into it. Xorg/libinput pick the device up through udev as a
 * touchscreen (INPUT_PROP_DIRECT), so the OSK receives genuine XI2 touch
 * events — the same path a physical touchscreen uses. This is the only way
 * the touch court can exercise the real kernel → libinput → Xorg → XI2 →
 * Ferrokey touch pipeline inside an isolated VM.
 *
 * A uinput device lives exactly as long as its fd stays open, so `create`
 * runs PERSISTENTLY: it creates the device and then serves commands from
 * stdin. Commands:
 *
 *   tap X Y     one tap (down + up) at (X, Y)
 *   down X Y    press and hold
 *   move X Y    move the held touch
 *   up          lift the touch
 *   destroy     destroy the device and exit
 *   quit        exit (device is destroyed on fd close)
 *
 * Usage from a court:
 *   sudo mkfifo /tmp/fake-touch.cmd
 *   sudo fake-touch create < /tmp/fake-touch.cmd >fake-touch.log 2>&1 &
 *   echo "tap 640 400" > /tmp/fake-touch.cmd
 *
 * Screen-relative coordinates: the device's ABS ranges are set to the
 * 1280x720 dummy screen, so a tap at (X, Y) lands at screen (X, Y).
 *
 * NOTE: this is a test-only helper. It lives in testing/ and is never
 * installed; /dev/uinput access is root-only inside the disposable VM.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/uinput.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/time.h>
#include <unistd.h>

#define DEVICE_NAME "Ferrokey Court Touchscreen"
#define SCREEN_W 1280
#define SCREEN_H 720
#define TOUCH_ID 100

static int ufd = -1;

static void die(const char *what) {
    fprintf(stderr, "fake-touch: %s: %s\n", what, strerror(errno));
    exit(1);
}

static void emit(int type, int code, int value) {
    struct input_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.type = type;
    ev.code = code;
    ev.value = value;
    if (gettimeofday(&ev.time, NULL) != 0)
        die("gettimeofday");
    if (write(ufd, &ev, sizeof(ev)) != (ssize_t)sizeof(ev))
        die("write event");
}

static void sync_events(void) { emit(EV_SYN, SYN_REPORT, 0); }

static void set_bit(int type, int bit) {
    if (ioctl(ufd, type, bit) < 0)
        die("ioctl set bit");
}

static void create_device(void) {
    ufd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (ufd < 0)
        die("open /dev/uinput");

    set_bit(UI_SET_EVBIT, EV_KEY);
    set_bit(UI_SET_KEYBIT, BTN_TOUCH);

    set_bit(UI_SET_EVBIT, EV_ABS);
    set_bit(UI_SET_ABSBIT, ABS_X);
    set_bit(UI_SET_ABSBIT, ABS_Y);
    set_bit(UI_SET_ABSBIT, ABS_MT_SLOT);
    set_bit(UI_SET_ABSBIT, ABS_MT_POSITION_X);
    set_bit(UI_SET_ABSBIT, ABS_MT_POSITION_Y);
    set_bit(UI_SET_ABSBIT, ABS_MT_TRACKING_ID);
    set_bit(UI_SET_ABSBIT, ABS_MT_PRESSURE);

    /* Direct touch: libinput must treat this as a touchscreen, and XI2 must
     * NOT generate emulated pointer events for it (no double delivery). */
    set_bit(UI_SET_PROPBIT, INPUT_PROP_DIRECT);

    /* Multi-touch protocol B axes. ABS_MT_PRESSURE must have a real range:
     * libinput rejects the whole device if any enabled axis has min == max
     * ("kernel bug: device has min == max on ABS_MT_PRESSURE"). */
    struct uinput_abs_setup abs[7];
    memset(abs, 0, sizeof(abs));
    abs[0].code = ABS_X;
    abs[0].absinfo.minimum = 0;
    abs[0].absinfo.maximum = SCREEN_W - 1;
    abs[1].code = ABS_Y;
    abs[1].absinfo.minimum = 0;
    abs[1].absinfo.maximum = SCREEN_H - 1;
    abs[2].code = ABS_MT_SLOT;
    abs[2].absinfo.minimum = 0;
    abs[2].absinfo.maximum = 9;
    abs[3].code = ABS_MT_POSITION_X;
    abs[3].absinfo.minimum = 0;
    abs[3].absinfo.maximum = SCREEN_W - 1;
    abs[4].code = ABS_MT_POSITION_Y;
    abs[4].absinfo.minimum = 0;
    abs[4].absinfo.maximum = SCREEN_H - 1;
    abs[5].code = ABS_MT_TRACKING_ID;
    abs[5].absinfo.minimum = 0;
    abs[5].absinfo.maximum = 65535;
    abs[6].code = ABS_MT_PRESSURE;
    abs[6].absinfo.minimum = 0;
    abs[6].absinfo.maximum = 255;
    for (int i = 0; i < 7; i++)
        if (ioctl(ufd, UI_ABS_SETUP, &abs[i]) < 0)
            die("ioctl UI_ABS_SETUP");

    struct uinput_setup setup;
    memset(&setup, 0, sizeof(setup));
    setup.id.bustype = BUS_USB;
    setup.id.vendor = 0x1FA8;  /* court vendor id */
    setup.id.product = 0x0001; /* court touchscreen */
    setup.id.version = 1;
    strncpy(setup.name, DEVICE_NAME, UINPUT_MAX_NAME_SIZE - 1);
    if (ioctl(ufd, UI_DEV_SETUP, &setup) < 0)
        die("ioctl UI_DEV_SETUP");
    if (ioctl(ufd, UI_DEV_CREATE) < 0)
        die("ioctl UI_DEV_CREATE");
    usleep(200 * 1000); /* let the kernel/udev settle */
    printf("fake-touch: device created\n");
}

static void tap_down(int x, int y) {
    emit(EV_ABS, ABS_MT_SLOT, 0);
    emit(EV_ABS, ABS_MT_TRACKING_ID, TOUCH_ID);
    emit(EV_ABS, ABS_MT_POSITION_X, x);
    emit(EV_ABS, ABS_MT_POSITION_Y, y);
    emit(EV_ABS, ABS_MT_PRESSURE, 60);
    emit(EV_KEY, BTN_TOUCH, 1);
    emit(EV_ABS, ABS_X, x);
    emit(EV_ABS, ABS_Y, y);
    sync_events();
}

static void tap_up(void) {
    emit(EV_KEY, BTN_TOUCH, 0);
    emit(EV_ABS, ABS_MT_TRACKING_ID, -1);
    emit(EV_ABS, ABS_MT_PRESSURE, 0);
    sync_events();
}

static void tap_move(int x, int y) {
    emit(EV_ABS, ABS_MT_POSITION_X, x);
    emit(EV_ABS, ABS_MT_POSITION_Y, y);
    emit(EV_ABS, ABS_X, x);
    emit(EV_ABS, ABS_Y, y);
    sync_events();
}

static void destroy_device(void) {
    if (ufd >= 0) {
        ioctl(ufd, UI_DEV_DESTROY);
        close(ufd);
        ufd = -1;
        printf("fake-touch: device destroyed\n");
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr,
                "usage: fake-touch create < /cmd-fifo   (persistent; commands:\n"
                "       tap X Y | down X Y | move X Y | up | destroy | quit)\n");
        return 2;
    }
    const char *cmd = argv[1];

    if (strcmp(cmd, "create") == 0) {
        create_device();
        /* Keep the uinput fd open and serve commands from stdin; the device
         * is destroyed when this process exits (fd close) or on `destroy`. */
        char line[128];
        while (fgets(line, sizeof(line), stdin)) {
            char c[16];
            int ax = -1, ay = -1;
            if (sscanf(line, "%15s %d %d", c, &ax, &ay) < 1)
                continue;
            if (strcmp(c, "tap") == 0) {
                if (ax < 0) continue;
                tap_down(ax, ay);
                usleep(120 * 1000);
                tap_up();
            } else if (strcmp(c, "down") == 0) {
                if (ax < 0) continue;
                tap_down(ax, ay);
            } else if (strcmp(c, "move") == 0) {
                if (ax < 0) continue;
                tap_move(ax, ay);
            } else if (strcmp(c, "up") == 0) {
                tap_up();
            } else if (strcmp(c, "destroy") == 0 || strcmp(c, "quit") == 0) {
                break;
            }
        }
        destroy_device();
        return 0;
    }

    fprintf(stderr, "fake-touch: unknown command '%s'\n", cmd);
    return 2;
}
