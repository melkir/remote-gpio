#!/usr/bin/env python3
import socket
import struct

CMD_WVCRE = 49
CMD_WVBSY = 32

COMMAND_NAMES = {
    0: "MODES",
    4: "WRITE",
    27: "WVCLR",
    28: "WVAG",
    32: "WVBSY",
    33: "WVHLT",
    49: "WVCRE",
    50: "WVDEL",
    51: "WVTX",
    53: "WVNEW",
}


def read_exact(conn, size):
    data = bytearray()
    while len(data) < size:
        chunk = conn.recv(size - len(data))
        if not chunk:
            return None
        data.extend(chunk)
    return bytes(data)


def main():
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 8888))
    server.listen(1)
    print("fake pigpiod listening on 127.0.0.1:8888", flush=True)

    while True:
        conn, addr = server.accept()
        peer = f"{addr[0]}:{addr[1]}"
        command_count = 0
        print(f"client connected: {peer}", flush=True)
        with conn:
            while True:
                header = read_exact(conn, 16)
                if header is None:
                    break
                command, p1, p2, ext_len = struct.unpack("<IIII", header)
                command_count += 1
                if ext_len:
                    extension = read_exact(conn, ext_len)
                    if extension is None:
                        break
                    ext_detail = f", ext={len(extension)} bytes"
                else:
                    ext_detail = ""

                if command == CMD_WVCRE:
                    result = 1
                elif command == CMD_WVBSY:
                    result = 0
                else:
                    result = 0

                name = COMMAND_NAMES.get(command, f"UNKNOWN_{command}")
                print(
                    f"{peer} {name} p1={p1} p2={p2}{ext_detail} -> {result}",
                    flush=True,
                )
                conn.sendall(struct.pack("<IIII", command, p1, p2, result))
        if command_count == 0:
            print(f"client disconnected: {peer} (no commands; likely doctor probe)", flush=True)
        else:
            print(f"client disconnected: {peer} ({command_count} commands)", flush=True)


if __name__ == "__main__":
    main()
