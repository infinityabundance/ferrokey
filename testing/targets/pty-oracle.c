/*
 * pty-oracle — the deterministic terminal-court probe (addendum §99).
 *
 * Runs as the PTY child of Ferrokey's embedded terminal workspace and
 * reports, over an AF_UNIX socket, everything it observes:
 *
 *   {"event":"start","pid":N,"rows":R,"cols":C}
 *   {"event":"winsize","rows":R,"cols":C}          (also on SIGWINCH)
 *   {"event":"input","hex":"..."}                  bytes read from stdin
 *   {"event":"exit","code":N}                      normal exit
 *   {"event":"signal","name":"SIGWINCH",...}       (reported via winsize)
 *
 * It puts stdin into raw mode, so the byte stream the OSK→PTY path delivers
 * arrives unmodified — the courts assert EXACT hex sequences. It also acts
 * as a scriptable terminal application: on complete input lines it responds,
 * which closes the loop (child → terminal parser → response → child):
 *
 *   clr           → write ESC[2J ESC[H        (clear + home the terminal)
 *   dsr           → write ESC[6n              (terminal answers ESC[r;cR)
 *   out.hello     → write "hello"
 *   flood         → write 100 "flood line i\n" lines
 *   alt.on/off    → write ESC[?1049h / l
 *   appc.on/off   → write ESC[?1h / l
 *   keypad.on     → write ESC=
 *   hostile       → write a batch of hostile escape sequences
 *   exit.7        → exit(7)
 *
 * The socket path comes from $PTY_ORACLE_SOCKET. The probe never echoes
 * input (raw mode), so typed bytes and terminal responses are unambiguous.
 */

#include <errno.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <termios.h>
#include <unistd.h>

static int sock = -1;

static void report(const char *fmt, ...) {
    if (sock < 0) return;
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if (n > (int)sizeof(buf)) n = sizeof(buf);
    ssize_t off = 0;
    while (off < n) {
        ssize_t w = write(sock, buf + off, (size_t)(n - off));
        if (w <= 0) break;
        off += w;
    }
}

static void report_winsize(void) {
    struct winsize ws;
    memset(&ws, 0, sizeof(ws));
    if (ioctl(0, TIOCGWINSZ, &ws) == 0) {
        report("{\"event\":\"winsize\",\"rows\":%u,\"cols\":%u}\n",
               ws.ws_row, ws.ws_col);
    }
}

static void on_winch(int sig) {
    (void)sig;
    report("{\"event\":\"signal\",\"name\":\"SIGWINCH\"}\n");
    report_winsize();
}

static void report_hex(const char *buf, size_t n) {
    /* Report the bytes as one compact hex event. */
    static char hex[8192 * 2 + 64];
    size_t cap = sizeof(hex) - 64;
    if (n * 2 > cap) n = cap / 2;
    static const char *digits = "0123456789abcdef";
    size_t j = 0;
    for (size_t i = 0; i < n; i++) {
        hex[j++] = digits[(buf[i] >> 4) & 0xf];
        hex[j++] = digits[buf[i] & 0xf];
    }
    hex[j] = '\0';
    report("{\"event\":\"input\",\"hex\":\"%s\"}\n", hex);
}

static void respond(const char *bytes, size_t n) {
    ssize_t off = 0;
    while (off < (ssize_t)n) {
        ssize_t w = write(1, bytes + off, n - (size_t)off);
        if (w <= 0) break;
        off += w;
    }
}

/* Respond with a NUL-terminated string (length computed correctly). */
static void respond_s(const char *s) {
    respond(s, strlen(s));
}

/* Suffix match: terminal responses (e.g. the DSR reply) may arrive on stdin
 * and pollute the buffer as a PREFIX; the command must still match when it
 * is the line's tail. */
static int line_ends_with(const char *line, const char *cmd) {
    size_t ll = strlen(line);
    size_t cl = strlen(cmd);
    if (ll < cl) return 0;
    return strcmp(line + (ll - cl), cmd) == 0;
}

int main(void) {
    const char *path = getenv("PTY_ORACLE_SOCKET");
    if (!path) {
        fprintf(stderr, "pty-oracle: PTY_ORACLE_SOCKET not set\n");
        return 2;
    }
    sock = socket(AF_UNIX, SOCK_STREAM, 0);
    if (sock < 0) {
        perror("pty-oracle: socket");
        return 2;
    }
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", path);
    /* Retry the connect: the listener may not be up yet. */
    int tries = 0;
    while (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0 && tries < 100) {
        usleep(50000);
        tries++;
    }
    if (tries >= 100) {
        fprintf(stderr, "pty-oracle: cannot connect to %s: %s\n", path, strerror(errno));
        return 2;
    }

    /* Raw mode on stdin so input arrives unmodified. */
    struct termios t;
    if (tcgetattr(0, &t) == 0) {
        cfmakeraw(&t);
        tcsetattr(0, TCSANOW, &t);
    }

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_winch;
    sigaction(SIGWINCH, &sa, NULL);

    report("{\"event\":\"start\",\"pid\":%d}\n", (int)getpid());
    report_winsize();

    char line[1024];
    size_t line_len = 0;
    for (;;) {
        char ch;
        ssize_t n = read(0, &ch, 1);
        if (n == 0) break; /* EOF */
        if (n < 0) {
            if (errno == EINTR) continue;
            break;
        }
        report_hex(&ch, 1);

        /* Any other control byte (ESC begins every terminal response): drop
         * the command buffer so responses never merge with commands. */
        if (ch < 0x20 && ch != '\r' && ch != '\n') {
            line_len = 0;
            continue;
        }

        /* Raw mode: Enter arrives as CR, never as LF. */
        if (ch == '\r' || ch == '\n' || line_len + 1 >= sizeof(line)) {
            line[line_len] = '\0';
            if (line_ends_with(line, "clr")) {
                respond_s("\x1b[2J\x1b[H");
            } else if (line_ends_with(line, "dsr")) {
                respond_s("\x1b[6n");
            } else if (line_ends_with(line, "out.hello")) {
                respond_s("hello");
            } else if (line_ends_with(line, "flood")) {
                char buf[64];
                for (int i = 0; i < 100; i++) {
                    int bl = snprintf(buf, sizeof(buf), "flood line %d\n", i);
                    respond(buf, (size_t)bl);
                }
            } else if (line_ends_with(line, "alt.on")) {
                respond_s("\x1b[?1049h");
            } else if (line_ends_with(line, "alt.off")) {
                respond_s("\x1b[?1049l");
            } else if (line_ends_with(line, "appc.on")) {
                respond_s("\x1b[?1h");
            } else if (line_ends_with(line, "appc.off")) {
                respond_s("\x1b[?1l");
            } else if (line_ends_with(line, "keypad.on")) {
                respond_s("\x1b=");
            } else if (line_ends_with(line, "hostile")) {
                respond_s("\x1b[9999;9999H\x1b[38;2;999;999;999m\x1b]52;c;AAAA\x07"
                          "\x1b[?9999h\x1b[2000~xxxxxxxx\x1b[999999b\xff\xfe");
            } else if (line_ends_with(line, "exit.7")) {
                report("{\"event\":\"exit\",\"code\":7}\n");
                return 7;
            }
            line_len = 0;
        } else {
            line[line_len++] = ch;
        }
    }
    report("{\"event\":\"exit\",\"code\":0}\n");
    return 0;
}
