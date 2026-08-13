// Court target: a Qt6 window with a QLineEdit.
//
// Reports focus and text changes over a Unix socket as JSON lines, matching
// the reporter protocol of the Rust targets (see ferrokey-test-common).

#include <QApplication>
#include <QLabel>
#include <QLineEdit>
#include <QTextStream>
#include <QTimer>
#include <QVBoxLayout>
#include <QWidget>

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <cstdio>
#include <cstring>

namespace {

int g_fd = -1;

void report(const QString &json) {
    if (g_fd < 0) return;
    QByteArray line = json.toUtf8();
    line.append('\n');
    ::write(g_fd, line.constData(), static_cast<size_t>(line.size()));
}

void reportFocus(bool focused) {
    report(QStringLiteral("{\"event\":\"focus\",\"focused\":%1}").arg(focused ? "true" : "false"));
}

void reportText(const QString &text) {
    QString escaped = text;
    escaped.replace('\\', "\\\\").replace('"', "\\\"");
    report(QStringLiteral("{\"event\":\"text\",\"text\":\"%1\"}").arg(escaped));
}

} // namespace

int main(int argc, char **argv) {
    QApplication app(argc, argv);

    // Reporter socket (same protocol as the Rust targets).
    const char *path = getenv("TARGET_SOCKET");
    if (path == nullptr) path = "/tmp/ferrokey-test-target.sock";
    ::unlink(path);
    int listener = ::socket(AF_UNIX, SOCK_STREAM, 0);
    if (listener >= 0) {
        struct sockaddr_un addr {};
        addr.sun_family = AF_UNIX;
        std::strncpy(addr.sun_path, path, sizeof(addr.sun_path) - 1);
        if (::bind(listener, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) == 0 &&
            ::listen(listener, 8) == 0) {
            // Accept one court client; keep it for the lifetime of the app.
            g_fd = ::accept(listener, nullptr, nullptr);
        }
    }
    report(QStringLiteral("{\"event\":\"ready\"}"));

    QWidget window;
    window.setWindowTitle("ferrokey-test-target-qt");
    window.resize(420, 120);

    auto *layout = new QVBoxLayout(&window);
    auto *label = new QLabel("ferrokey test target (qt)");
    auto *edit = new QLineEdit;

    QObject::connect(edit, &QLineEdit::textChanged, [](const QString &t) { reportText(t); });

    layout->addWidget(label);
    layout->addWidget(edit);
    window.show();
    window.activateWindow();
    // Give the field keyboard focus before the court activates the window so
    // that activation deterministically reports focused:true (Qt restores the
    // previous focus widget when a window is (re)activated).
    edit->setFocus();

    // Qt has no single "this window owns the keyboard focus" signal that also
    // covers widget-level focus restoration, so poll the canonical state:
    // the window is the active window AND the keyboard focus widget belongs
    // to this window. This mirrors the GTK target's entry-focus semantics.
    // (QWidget::hasFocus() would be wrong here: it is only true for the focus
    // widget itself, and the QLineEdit holds focus, not the QWidget.)
    QTimer focusTimer;
    bool wasFocused = false;
    QObject::connect(&focusTimer, &QTimer::timeout, [&]() {
        QWidget *fw = QApplication::focusWidget();
        bool active = window.isActiveWindow();
        bool focused = active && fw != nullptr && fw->window() == &window;
        if (focused != wasFocused) {
            wasFocused = focused;
            reportFocus(focused);
            std::fprintf(stderr,
                         "[qt-target] focus %s (active=%d focusWidget=%s)\n",
                         focused ? "true" : "false", active ? 1 : 0,
                         fw ? fw->metaObject()->className() : "null");
            std::fflush(stderr);
        }
    });
    focusTimer.start(50);

    return app.exec();
}
