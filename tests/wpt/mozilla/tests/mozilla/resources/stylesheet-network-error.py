# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

stylesheet = b"#target { color: rgb(0, 128, 0); }\n"


def main(request, response):
    response.add_required_headers = False
    output = b"HTTP/1.1 200 OK\r\n"
    output += b"Content-Type: text/css\r\n"
    output += b"Connection: close\r\n"
    if request.GET.first(b"error") == b"decode":
        output += b"Content-Encoding: gzip\r\n"
        output += b"Content-Length: %d\r\n" % len(stylesheet)
    else:
        output += b"Content-Length: %d\r\n" % (len(stylesheet) + 8)
    output += b"\r\n"
    output += stylesheet
    response.writer.write(output)
    response.close_connection = True
