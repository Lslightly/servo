# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

document = b"<!doctype html><p>cut short"


def main(request, response):
    response.add_required_headers = False
    output = b"HTTP/1.1 200 OK\r\n"
    output += b"Content-Type: text/html\r\n"
    output += b"Connection: close\r\n"
    output += b"Content-Length: %d\r\n" % (len(document) + 8)
    output += b"\r\n"
    output += document
    response.writer.write(output)
    response.close_connection = True
