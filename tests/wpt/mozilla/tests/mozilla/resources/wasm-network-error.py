# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# An empty module, which compiles if a consumer takes a body that ends early as complete.
module = b"\0asm\x01\0\0\0"


def main(request, response):
    response.add_required_headers = False
    output = b"HTTP/1.1 200 OK\r\n"
    output += b"Content-Type: application/wasm\r\n"
    output += b"Connection: close\r\n"
    if request.GET.first(b"error") == b"decode":
        output += b"Content-Encoding: gzip\r\n"
        output += b"Content-Length: %d\r\n" % len(module)
    else:
        output += b"Content-Length: %d\r\n" % (len(module) + 8)
    output += b"\r\n"
    output += module
    response.writer.write(output)
    response.close_connection = True
