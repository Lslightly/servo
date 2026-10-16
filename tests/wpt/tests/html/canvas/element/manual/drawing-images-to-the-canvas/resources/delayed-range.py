import os
import re
import time


def main(request, response):
    name = os.path.basename(request.GET.first(b"name").decode())
    with open(os.path.join(request.doc_root, "media", name), "rb") as f:
        data = f.read()

    headers = [
        (b"Content-Type", b"video/" + name.rsplit(".", 1)[1].encode()),
        (b"Accept-Ranges", b"bytes"),
    ]

    range_header = request.headers.get(b"range")
    if not range_header:
        return 200, headers, data

    start = int(re.match(r"bytes=(\d*)-", range_header.decode()).group(1) or 0)
    if start > 0:
        # Delay the answer to a request in the middle of the resource, like the one that a seek
        # makes, so that the media element waits for the headers of the response for a while.
        time.sleep(float(request.GET.first(b"delay", b"2")))
    headers.append((b"Content-Range", f"bytes {start}-{len(data) - 1}/{len(data)}".encode()))
    return 206, headers, data[start:]
