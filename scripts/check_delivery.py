#!/usr/bin/env python3
"""Check real delivery in a private KWin session. Never submit a Codex prompt.

Requires KWin, dbus-run-session, PyGObject, dbus-python, wl-copy, and Kitty
for terminal targets. Build the delivery_probe example first.
"""
import argparse
import array
import fcntl
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import termios
import threading
import time


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("gtk", "kitty", "codex"), default="gtk")
    parser.add_argument("--legacy", action="store_true", help="reproduce the old unpaced typing path")
    parser.add_argument("--unicode", action="store_true", help="include text absent from a US keymap")
    parser.add_argument("--repeats", type=int, default=30)
    parser.add_argument("--inner", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 1000:
        parser.error("--repeats must be between 1 and 1000")
    return args


def forward(source, destination):
    """Forward EI packets and their keymap file descriptors without coupling directions."""
    try:
        while True:
            data, ancillary, flags, _ = source.recvmsg(65536, socket.CMSG_SPACE(256))
            descriptors = []
            for level, kind, payload in ancillary:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    received = array.array("i")
                    received.frombytes(payload)
                    descriptors.extend(received)
            try:
                if not data:
                    return
                if flags & socket.MSG_CTRUNC:
                    raise RuntimeError("truncated EI file descriptors")
                sent = destination.sendmsg([data], ancillary)
                destination.sendall(data[sent:])
            finally:
                for fd in descriptors:
                    os.close(fd)
    except (ConnectionError, OSError):
        # The compositor and probe deliberately close at the end of each run.
        return


def main(args):
    if not args.inner:
        root = Path(tempfile.mkdtemp(prefix="nvstt-delivery-"))
        env = dict(os.environ, XDG_RUNTIME_DIR=str(root), XDG_CONFIG_HOME=str(root / "config"),
                   XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"),
                   WAYLAND_DISPLAY="nvstt-test", QT_QPA_PLATFORM="offscreen", KWIN_COMPOSE="Q",
                   GDK_BACKEND="wayland", GTK_A11Y="none", GSK_RENDERER="cairo",
                   NVSTT_PRIVATE_DELIVERY_TEST="1", LIBEI_SOCKET=str(root / "ei.sock"))
        env.pop("DISPLAY", None)
        env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        env.pop("NVSTT_PROBE_LEGACY", None)
        if args.legacy:
            env["NVSTT_PROBE_LEGACY"] = "1"
        with (root / "session.log").open("w") as log:
            run = subprocess.Popen(["dbus-run-session", "--", sys.executable, __file__,
                                    *sys.argv[1:], "--inner"], env=env, stdout=log, stderr=log,
                                   start_new_session=True)
            try:
                code = run.wait(timeout=90)
            finally:
                # Include any D-Bus activated helpers in cleanup, even after
                # the inner runner exits or times out.
                try:
                    os.killpg(run.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                run.wait(timeout=5)
        report = root / "result.json"
        print(report.read_text() if report.exists() else "Harness failed; inspect session.log")
        print(f"Artifacts: {root}")
        return code

    import dbus

    root = Path(os.environ["XDG_RUNTIME_DIR"])
    assert root.name.startswith("nvstt-delivery-")
    assert os.environ["NVSTT_PRIVATE_DELIVERY_TEST"] == "1"
    processes = []
    sockets = []
    try:
        kwin = subprocess.Popen(["kwin_wayland", "--virtual", "--socket", "nvstt-test",
                                 "--no-lockscreen", "--no-global-shortcuts", "--no-kactivities"],
                                stdout=(root / "kwin.log").open("w"), stderr=subprocess.STDOUT)
        processes.append(kwin)
        for _ in range(100):
            if (root / "nvstt-test").exists():
                break
            if kwin.poll() is not None:
                raise RuntimeError("private KWin exited")
            time.sleep(.05)
        bus = dbus.SessionBus()
        for _ in range(100):
            if bus.name_has_owner("org.kde.KWin"):
                break
            if kwin.poll() is not None:
                raise RuntimeError("private KWin exited before registering D-Bus")
            time.sleep(.05)
        else:
            raise RuntimeError("private KWin did not register D-Bus")
        remote = bus.get_object("org.kde.KWin", "/org/kde/KWin/EIS/RemoteDesktop")
        descriptor, _cookie = remote.connectToEIS(
            dbus.Int32(1), dbus_interface="org.kde.KWin.EIS.RemoteDesktop")
        server = socket.socket(fileno=descriptor.take())
        server.setblocking(True)
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(root / "ei.sock"))
        listener.listen(1)
        sockets.extend([server, listener])

        def bridge():
            client, _ = listener.accept()
            sockets.append(client)
            threading.Thread(target=forward, args=(server, client), daemon=True).start()
            forward(client, server)
        threading.Thread(target=bridge, daemon=True).start()

        # GTK opens the display on import. Import only after KWin is ready.
        import gi
        gi.require_version("Gtk", "4.0")
        from gi.repository import Gtk, GLib

        phrase = "A long dictation needs every word, number 123 and final punctuation. "
        if args.unicode:
            phrase = "Rupees ₹1,234.50, naïve café, हिंदी, C++ and .env.\n"
        expected = phrase * args.repeats + "Final boundary."
        (root / "input.txt").write_text(expected)
        loop = GLib.MainLoop()
        if args.target == "gtk":
            Gtk.init()
            window = Gtk.Window(title="nvstt private delivery test")
            view = Gtk.TextView()
            window.set_child(view)
            window.set_default_size(800, 600)
            window.present()
            view.grab_focus()

            def get_text():
                buffer = view.get_buffer()
                return buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), True)
        else:
            receiver = root / "receive.py"
            receiver.write_text(
                "import os,tty,pathlib\ntty.setraw(0)\nos.write(1,b'\\x1b[?2004h')\n"
                f"with pathlib.Path({str(root / 'terminal.txt')!r}).open('wb',buffering=0) as f:\n"
                " while True:\n  data=os.read(0,4096)\n  if not data: break\n  f.write(data)\n")
            child = (["codex", "--no-daemon", "--no-alt-screen", "-C", str(Path.cwd())]
                     if args.target == "codex" else [sys.executable, str(receiver)])
            terminal = subprocess.Popen(
                ["kitty", "--config", "NONE", "-o", "linux_display_server=wayland",
                 "-o", "confirm_os_window_close=0", "-o", "allow_remote_control=yes",
                 "--listen-on", "unix:" + str(root / "kitty.sock"), *child],
                stdout=(root / "terminal.log").open("w"), stderr=subprocess.STDOUT)
            processes.append(terminal)

            def kitty(*command):
                return subprocess.run(["kitty", "@", "--to", "unix:" + str(root / "kitty.sock"),
                                       *command], capture_output=True, text=True, timeout=3,
                                      check=True).stdout

            if args.target == "codex":
                for _ in range(150):
                    if (root / "kitty.sock").exists() and "›" in kitty("get-text"):
                        break
                    time.sleep(.1)
                else:
                    raise RuntimeError("Codex composer did not become ready")
                get_text = lambda: kitty("get-text")
            else:
                def get_text():
                    if not (root / "terminal.txt").exists():
                        return ""
                    text = (root / "terminal.txt").read_text()
                    # Require exactly one bracketed paste. read_text translates
                    # terminal CR/CRLF to LF, matching the fixture's newlines.
                    start, end = "\x1b[200~", "\x1b[201~"
                    if (text.startswith(start) and text.endswith(end)
                            and text.count(start) == 1 and text.count(end) == 1):
                        return text[len(start):-len(end)]
                    return text if args.legacy else "[missing bracketed-paste boundaries]"

        probes = []
        def launch():
            binary = Path(__file__).resolve().parent.parent / "target/debug/examples/delivery_probe"
            probe = subprocess.Popen([str(binary), str(root / "input.txt")],
                                     stdout=(root / "probe.log").open("w"), stderr=subprocess.STDOUT)
            probes.append(probe)
            processes.append(probe)
            return False
        GLib.timeout_add(1000, launch)

        verdict = []
        def finish():
            try:
                actual = get_text()
                (root / "actual.txt").write_text(actual)
                equal = actual == expected
                pending = None
                if args.target == "codex":
                    tree = json.loads(kitty("ls"))
                    pid = tree[0]["tabs"][0]["windows"][0]["foreground_processes"][0]["pid"]
                    fd = os.open(f"/proc/{pid}/fd/0", os.O_RDONLY | os.O_NONBLOCK | os.O_NOCTTY)
                    try:
                        count = array.array("i", [0])
                        fcntl.ioctl(fd, termios.FIONREAD, count, True)
                        pending = count[0]
                    finally:
                        os.close(fd)
                    counts = re.findall(r"\[Pasted [Cc]ontent (\d+) chars\]", actual)
                    equal = counts == [str(len(expected))] and pending == 0
                    # No extra key is sent until after the verdict and capture.
                    if args.legacy:
                        kitty("send-key", "space")
                        time.sleep(.4)
                        (root / "after-key.txt").write_text(get_text())
                passed = equal and len(probes) == 1 and probes[0].poll() == 0
                report = dict(target=args.target, legacy=args.legacy, expected_chars=len(expected),
                              exact_text=equal if args.target != "codex" else None,
                              complete_paste=equal if args.target == "codex" else None,
                              unread_terminal_bytes=pending, passed=passed)
                (root / "result.json").write_text(json.dumps(report, indent=2))
                verdict.append(passed)
            finally:
                loop.quit()
            return False
        GLib.timeout_add(10000, finish)
        loop.run()
        if args.target == "gtk":
            window.destroy()
        return 0 if verdict == [True] else 1
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for stream in sockets:
            stream.close()


if __name__ == "__main__":
    sys.exit(main(arguments()))
