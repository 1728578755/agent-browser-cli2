//! 极简图片型 PDF 生成器：把一张 JPEG 包成单页 PDF。
//!
//! 用途只有一个：页面禁止调试器注入时（`Page.printToPDF` 不可用），
//! daemon 用整页拼图（`captureFullPage`）得到的 JPEG 合成 PDF 交付。
//! 文字不可选中，语义上是「整页截图」而不是真正的排版 PDF。
//!
//! 二进制安全是这里的硬要求：JPEG 的字节必须原封不动地写进内容流
//! （`/Filter /DCTDecode`），任何经过 `String` 的路径都会破坏 0x80 以上的字节。

use anyhow::{anyhow, Result};

/// 把 JPEG 包装成单页 PDF。
///
/// 页面尺寸按 96dpi 换算（CSS px → pt）：`pt = px * 72 / 96`。
/// 输出结构固定 5 个对象：Catalog / Pages / Page / 内容流 / 图像 XObject，
/// 末尾写交叉引用表与 `startxref`，偏移量全部按真实字节位置计算。
pub(crate) fn image_pdf(jpeg: &[u8], width_px: u32, height_px: u32) -> Result<Vec<u8>> {
    if jpeg.is_empty() {
        return Err(anyhow!("image_pdf: JPEG 数据为空，拒绝生成 PDF"));
    }
    if width_px == 0 || height_px == 0 {
        return Err(anyhow!(
            "image_pdf: 页面尺寸非法（{width_px}x{height_px}），拒绝生成 PDF"
        ));
    }
    let width_pt = f64::from(width_px) * 72.0 / 96.0;
    let height_pt = f64::from(height_px) * 72.0 / 96.0;
    let content = format!("q {width_pt:.2} 0 0 {height_pt:.2} 0 0 cm /Im0 Do Q\n");

    let mut out: Vec<u8> = Vec::with_capacity(jpeg.len() + 1024);
    // 对象 1..=5 的字节偏移，xref 表按顺序写；偏移算错 PDF 就打不开。
    let mut offsets: Vec<usize> = Vec::with_capacity(5);

    // `%` 注释行里的 4 个高位字节是 PDF 的「二进制文件」标记，阅读器据此按二进制处理。
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width_pt:.2} {height_pt:.2}] \
             /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );

    offsets.push(out.len());
    out.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    out.extend_from_slice(content.as_bytes());
    out.extend_from_slice(b"endstream\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Image /Width {width_px} /Height {height_px} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .as_bytes(),
    );
    // 关键：JPEG 原样落盘，长度就是字节数（不是字符数）。
    out.extend_from_slice(jpeg);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = out.len();
    let size = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一段结构上像 JPEG 的假数据：SOI + APP0 头 + 填充 + EOI。
    fn fake_jpeg() -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        // 0x80 以上的字节必须原样保留，用来抓「JPEG 被当字符串处理」的回归。
        bytes.extend((0u16..512).map(|index| (index % 256) as u8));
        bytes.extend_from_slice(&[0xff, 0xd9]);
        bytes
    }

    #[test]
    fn image_pdf_has_valid_header_and_lengths() {
        let jpeg = fake_jpeg();
        let pdf = image_pdf(&jpeg, 1280, 2000).expect("生成 PDF");
        assert!(pdf.starts_with(b"%PDF-1.4"), "PDF 头不正确");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/DCTDecode"), "缺少 DCTDecode 过滤器");
        assert!(text.contains("/Width 1280"), "图像宽度未写入");
        assert!(text.contains("/Height 2000"), "图像高度未写入");
        // 96dpi 换算：1280px → 960pt，2000px → 1500pt。
        assert!(
            text.contains("/MediaBox [0 0 960.00 1500.00]"),
            "页面尺寸未换算"
        );
        assert!(
            text.contains(&format!("/Length {}", jpeg.len())),
            "图像流长度必须是真实字节数"
        );
        assert!(pdf.ends_with(b"%%EOF\n"), "缺少 %%EOF 结尾");
        // 交叉引用表偏移必须真的指向 `xref`，否则任何阅读器都会报损坏。
        let startxref = text.rfind("startxref").expect("缺少 startxref");
        let offset_text = text[startxref + "startxref".len()..]
            .split_whitespace()
            .next()
            .expect("缺少 startxref 偏移");
        let offset: usize = offset_text.parse().expect("startxref 偏移不是数字");
        assert_eq!(
            &pdf[offset..offset + 4],
            b"xref",
            "startxref 偏移 {offset} 没有指向交叉引用表"
        );
        // JPEG 字节必须逐字节保留在流里（二进制安全）。
        assert!(
            pdf.windows(jpeg.len())
                .any(|window| window == jpeg.as_slice()),
            "JPEG 数据在写入过程中被改动"
        );
        // xref 每一行的前 10 位是该对象的真实字节偏移，偏一位阅读器就找不到对象。
        for index in 1..=5 {
            let marker = format!("{index} 0 obj");
            let object_offset = pdf
                .windows(marker.len())
                .position(|window| window == marker.as_bytes())
                .unwrap_or_else(|| panic!("互引对象 {index} 缺失"));
            assert!(
                text.contains(&format!("{object_offset:010} 00000 n")),
                "对象 {index} 的 xref 偏移与实际位置不一致"
            );
        }
    }

    #[test]
    fn image_pdf_rejects_empty() {
        assert!(image_pdf(&[], 1280, 2000).is_err());
        assert!(image_pdf(&[0xff, 0xd8], 0, 2000).is_err());
    }
}
