# Offline QR dependency

ZXing core 3.5.4, Apache License 2.0. Source: https://github.com/zxing/zxing/tree/zxing-3.5.4

The build obtains the unchanged JAR from Maven Central and verifies its pinned SHA-256:
`71de5d89341b5fcf5dd89da7f44e84d825d0e084cdf3ec77c9abe26b0f0ceb13`.

The APK packages the decoder and the license/notice under assets. Camera images are decoded on-device. No QR service, network upload, external scanner intent, URL handler, or Google Play Services is used.
