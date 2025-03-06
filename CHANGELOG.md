0.1.1 (2025-03-06)
==================
This is a new patch release that fixes cryptographic handshake failures during
connection and fixes compilation from read-only source trees with new `capnp`
versions.  It also removes the `openssl` dependency and allows reading the
request body before sending response headers.

New features:

* [FEATURE #1](https://github.com/apersomany/libcfd/pull/1):
Switch reqwest from native-tls to rustls.
* [FEATURE #2](https://github.com/apersomany/libcfd/pull/2):
Allow reading request body before sending response headers.

Bug fixes:

* [BUG #3](https://github.com/apersomany/libcfd/pull/3):
Set APLN to fix TLS handshake.
* [BUG #4](https://github.com/apersomany/libcfd/issues/4):
Move `capnp`-generated files into `$OUT_DIR`.

0.1.0 (2024-02-09)
==================
Initial release.
