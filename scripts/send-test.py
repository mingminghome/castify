#!/usr/bin/env python3
"""Discover a castify MediaRenderer on the LAN and send a test photo.

Usage:
  ./scripts/send-test.py
  ./scripts/send-test.py --name "Living Room"
  ./scripts/send-test.py --file /path/to/photo.jpg
"""

from __future__ import annotations

import argparse
import http.server
import os
import socket
import struct
import sys
import threading
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
import zlib
from functools import partial
from typing import Optional

SSDP_ADDR = ("239.255.255.250", 1900)
MSEARCH = (
    "M-SEARCH * HTTP/1.1\r\n"
    "HOST: 239.255.255.250:1900\r\n"
    'MAN: "ssdp:discover"\r\n'
    "MX: 2\r\n"
    "ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n"
    "\r\n"
).encode("ascii")

SOAP_NS = "http://schemas.xmlsoap.org/soap/envelope/"
AV_NS = "urn:schemas-upnp-org:service:AVTransport:1"


def local_ipv4() -> str:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect(("8.8.8.8", 80))
        return sock.getsockname()[0]
    finally:
        sock.close()


def make_test_png(path: str) -> None:
    width, height = 640, 360

    def pixel(x: int, y: int) -> bytes:
        stripe = (x // 40) % 2
        if y < 48:
            return bytes((20, 20, 24))
        if stripe:
            return bytes((32, 132, 255))
        return bytes((255, 196, 32))

    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(pixel(x, y))
    compressed = zlib.compress(bytes(raw), 9)

    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", compressed) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def start_file_server(directory: str, filename: str, bind_ip: str, port: int = 0) -> tuple[http.server.HTTPServer, str]:
    handler = partial(http.server.SimpleHTTPRequestHandler, directory=directory)
    httpd = http.server.HTTPServer((bind_ip, port), handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    port = httpd.server_address[1]
    url = f"http://{bind_ip}:{port}/{filename}"
    return httpd, url


def parse_ssdp(packet: bytes) -> dict[str, str]:
    text = packet.decode("utf-8", "replace")
    headers: dict[str, str] = {}
    for line in text.split("\r\n"):
        if ":" in line:
            key, value = line.split(":", 1)
            headers[key.strip().upper()] = value.strip()
    return headers


def ssdp_search(timeout: float = 3.0) -> list[str]:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.settimeout(timeout)
    sock.bind(("", 0))
    sock.sendto(MSEARCH, SSDP_ADDR)
    locations: list[str] = []
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            data, _addr = sock.recvfrom(4096)
        except socket.timeout:
            break
        headers = parse_ssdp(data)
        loc = headers.get("LOCATION")
        if loc and loc not in locations:
            locations.append(loc)
    sock.close()
    return locations


def fetch(url: str, timeout: float = 3.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "castify-send-test/0.1"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", "replace")


def xml_local(tag: str) -> str:
    if "}" in tag:
        return tag.rsplit("}", 1)[1]
    return tag


def parse_device(xml_text: str, base_url: str) -> dict[str, str]:
    root = ET.fromstring(xml_text)
    info = {
        "friendlyName": "",
        "deviceType": "",
        "controlURL": "",
        "base": base_url.rsplit("/", 1)[0],
    }
    for node in root.iter():
        name = xml_local(node.tag)
        if name == "friendlyName" and node.text:
            info["friendlyName"] = node.text.strip()
        elif name == "deviceType" and node.text:
            info["deviceType"] = node.text.strip()
        elif name == "controlURL" and node.text:
            parent_type = ""
            # Walk siblings: the enclosing <service> has serviceType
        elif name == "serviceType" and node.text and "AVTransport" in node.text:
            parent = node.getparent() if hasattr(node, "getparent") else None
            # ElementTree has no getparent; scan service blocks instead.
    for service in root.iter():
        if xml_local(service.tag) != "service":
            continue
        stype = ""
        control = ""
        for child in list(service):
            if xml_local(child.tag) == "serviceType" and child.text:
                stype = child.text.strip()
            if xml_local(child.tag) == "controlURL" and child.text:
                control = child.text.strip()
        if "AVTransport" in stype and control:
            if control.startswith("http"):
                info["controlURL"] = control
            else:
                info["controlURL"] = info["base"].rstrip("/") + "/" + control.lstrip("/")
    return info


def soap_action(control_url: str, action: str, inner: str) -> str:
    body = (
        '<?xml version="1.0" encoding="utf-8"?>'
        f'<s:Envelope xmlns:s="{SOAP_NS}" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">'
        f'<s:Body><u:{action} xmlns:u="{AV_NS}">{inner}</u:{action}></s:Body></s:Envelope>'
    ).encode("utf-8")
    req = urllib.request.Request(
        control_url,
        data=body,
        method="POST",
        headers={
            "CONTENT-TYPE": 'text/xml; charset="utf-8"',
            "SOAPACTION": f'"{AV_NS}#{action}"',
            "CONTENT-LENGTH": str(len(body)),
            "USER-AGENT": "castify-send-test/0.1",
        },
    )
    with urllib.request.urlopen(req, timeout=5) as resp:
        return resp.read().decode("utf-8", "replace")


def send_uri(control_url: str, media_url: str, title: str, mime: str) -> None:
    didl = (
        "&lt;DIDL-Lite xmlns=&quot;urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/&quot;"
        " xmlns:dc=&quot;http://purl.org/dc/elements/1.1/&quot;"
        " xmlns:upnp=&quot;urn:schemas-upnp-org:metadata-1-0/upnp/&quot;&gt;"
        "&lt;item id=&quot;0&quot; parentID=&quot;-1&quot; restricted=&quot;1&quot;&gt;"
        f"&lt;dc:title&gt;{title}&lt;/dc:title&gt;"
        "&lt;upnp:class&gt;object.item.imageItem.photo&lt;/upnp:class&gt;"
        f"&lt;res protocolInfo=&quot;http-get:*:{mime}:*&quot;&gt;{media_url}&lt;/res&gt;"
        "&lt;/item&gt;&lt;/DIDL-Lite&gt;"
    )
    inner = (
        "<InstanceID>0</InstanceID>"
        f"<CurrentURI>{media_url}</CurrentURI>"
        f"<CurrentURIMetaData>{didl}</CurrentURIMetaData>"
    )
    soap_action(control_url, "SetAVTransportURI", inner)
    soap_action(control_url, "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>")


def discover_named(name_substr: str, timeout: float) -> dict[str, str]:
    print(f"searching {timeout:.0f}s for MediaRenderer matching {name_substr!r} …")
    locations = ssdp_search(timeout)
    if not locations:
        raise SystemExit("no MediaRenderer answered SSDP. Start a renderer on this network first.")
    matched: Optional[dict[str, str]] = None
    for loc in locations:
        try:
            xml_text = fetch(loc)
        except (urllib.error.URLError, TimeoutError) as exc:
            print(f"  skip {loc}: {exc}")
            continue
        info = parse_device(xml_text, loc)
        info["location"] = loc
        print(f"  found {info['friendlyName']!r} at {loc}")
        if name_substr.lower() in info["friendlyName"].lower():
            matched = info
            break
        if matched is None:
            matched = info
    if not matched or not matched.get("controlURL"):
        raise SystemExit("found devices, but none exposed AVTransport")
    return matched


def main() -> int:
    parser = argparse.ArgumentParser(description="Send a test photo to a castify renderer")
    parser.add_argument("--name", default="castify", help="friendly name substring")
    parser.add_argument("--file", help="existing image to send (png/jpg)")
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("--hold", type=float, default=20.0, help="seconds to keep the HTTP server up so the renderer can fetch the file")
    parser.add_argument("--control-url", help="skip SSDP and POST SOAP to this AVTransport URL")
    parser.add_argument(
        "--fetch-host",
        help="address the renderer uses to fetch the file (default: this machine's LAN address)",
    )
    parser.add_argument("--http-port", type=int, default=0, help="bind the file server to this port (0 = ephemeral)")
    args = parser.parse_args()

    bind_ip = local_ipv4()
    workdir = os.path.join(os.path.dirname(__file__), "..", "target", "send-test")
    os.makedirs(workdir, exist_ok=True)
    if args.file:
        src = os.path.abspath(args.file)
        filename = os.path.basename(src)
        dest = os.path.join(workdir, filename)
        with open(src, "rb") as inf, open(dest, "wb") as outf:
            outf.write(inf.read())
        mime = "image/jpeg" if filename.lower().endswith((".jpg", ".jpeg")) else "image/png"
    else:
        filename = "test.png"
        dest = os.path.join(workdir, filename)
        make_test_png(dest)
        mime = "image/png"

    httpd, media_url = start_file_server(os.path.abspath(workdir), filename, "0.0.0.0", args.http_port)
    if not args.fetch_host:
        port = httpd.server_address[1]
        media_url = f"http://{bind_ip}:{port}/{filename}"
    if args.fetch_host:
        port = httpd.server_address[1]
        media_url = f"http://{args.fetch_host}:{port}/{filename}"
    print(f"serving {media_url}")
    try:
        if args.control_url:
            control_url = args.control_url
            print(f"sending to {control_url}")
        else:
            device = discover_named(args.name, args.timeout)
            control_url = device["controlURL"]
            print(f"sending to {device['friendlyName']}  {control_url}")
        send_uri(control_url, media_url, filename, mime)
        print("SetAVTransportURI + Play sent. Holding the HTTP server so the renderer can fetch the file …")
        time.sleep(args.hold)
        print("done")
        return 0
    except urllib.error.HTTPError as exc:
        print(f"SOAP HTTP {exc.code}: {exc.read()[:400]!r}", file=sys.stderr)
        return 1
    finally:
        httpd.shutdown()


if __name__ == "__main__":
    raise SystemExit(main())
