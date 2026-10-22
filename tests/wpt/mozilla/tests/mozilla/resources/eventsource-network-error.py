# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.


def main(request, response):
    response.add_required_headers = False
    event = b"retry: 10\ndata: event\n\n"
    response.writer.write(
        b"HTTP/1.1 200 OK\r\n"
        b"Content-Type: text/event-stream\r\n"
        b"Transfer-Encoding: chunked\r\n"
        b"Connection: close\r\n"
        b"\r\n"
        b"%x\r\n%s\r\n"
        # The connection closes in the middle of the next chunk.
        b"100\r\ndata: cut"
        % (len(event), event)
    )
    response.close_connection = True
