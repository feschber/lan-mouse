#!/usr/bin/env python3
"""Isolated test-only Wayland proxy: pause this test connection once, never the compositor."""
import array, os, select, socket, sys, threading, time
from pathlib import Path
endpoint, upstream, flag = sys.argv[1:]
if Path(endpoint).exists():
    raise SystemExit('Refusing to overwrite an existing socket')
listener = socket.socket(socket.AF_UNIX)
listener.bind(endpoint)
listener.listen()
print('READY isolated Wayland stall proxy', flush=True)

def forward(source, destination, stall):
    try:
        while True:
            if stall and Path(flag).exists():
                Path(flag).unlink()
                print('STALL test input connection for 1200ms', flush=True)
                time.sleep(1.2)
            if not select.select([source], [], [], .01)[0]:
                continue
            data, ancillary, flags, _ = source.recvmsg(65536, socket.CMSG_SPACE(4096), socket.MSG_CMSG_CLOEXEC)
            if not data:
                return
            received = []
            for level, kind, payload in ancillary:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    fds = array.array('i'); fds.frombytes(payload)
                    received.extend(fds)
            try:
                if flags & socket.MSG_CTRUNC:
                    raise RuntimeError('truncated ancillary descriptors')
                sent = destination.sendmsg([data], ancillary)
                destination.sendall(data[sent:])
            finally:
                for fd in received:
                    os.close(fd)
    except (OSError, ValueError):
        pass
    finally:
        for connection in (source, destination):
            try: connection.shutdown(socket.SHUT_RDWR)
            except OSError: pass

def pair(client):
    server = socket.socket(socket.AF_UNIX)
    try:
        server.connect(upstream)
        t = threading.Thread(target=forward, args=(client, server, True), daemon=True)
        t.start()
        forward(server, client, False)
        t.join()
    finally:
        client.close(); server.close()

try:
    while True:
        client, _ = listener.accept()
        threading.Thread(target=pair, args=(client,), daemon=True).start()
finally:
    listener.close()
    Path(endpoint).unlink(missing_ok=True)
