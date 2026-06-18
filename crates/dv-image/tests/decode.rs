//! Decode the committed 2x2 RGBA fixture and check pixels/alpha.

#[test]
fn decodes_rgba_png() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/test.png");
    let bytes = std::fs::read(path).expect("read fixture");
    let img = dv_image::decode(&bytes).expect("decode png");

    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(img.rgba.len(), 2 * 2 * 4);
    // Pixel 0 = opaque red, pixel 3 = fully transparent.
    assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(img.rgba[15], 0, "transparent corner alpha");
}

#[test]
fn rejects_unsupported() {
    assert!(dv_image::decode(b"GIF89a....").is_none());
    assert!(dv_image::decode(&[]).is_none());
}
