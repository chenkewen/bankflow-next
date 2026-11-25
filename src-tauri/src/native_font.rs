use pdfium_render::prelude::*;
use std::error::Error;

fn use_font_mapping(object: &PdfPageObject) -> Result<(), Box<dyn Error>> {
    let bindings = object.bindings();
    let handle = bindings.get_handle_from_object(object);
    // 标记句柄归当前页对象所有。本次只删除 ActualText 属性，保留标记及对象的所有权。
    unsafe {
        let count = bindings.FPDFPageObj_CountMarks(handle);
        if count < 0 {
            return Err("无法读取 PDF 内容标记".into());
        }
        for index in 0..count {
            let mark = bindings.FPDFPageObj_GetMark(handle, index as _);
            if mark.is_null() {
                return Err("无法读取 PDF 内容标记".into());
            }
            if bindings.FPDFPageObjMark_GetParamValueType(mark, "ActualText") != 0
                && !bindings.is_true(bindings.FPDFPageObjMark_RemoveParam(
                    handle,
                    mark,
                    "ActualText",
                ))
            {
                return Err("无法还原 PDF 字体文字映射".into());
            }
        }
    }
    if let PdfPageObject::XObjectForm(form) = object {
        for child in form.iter() {
            use_font_mapping(&child)?;
        }
    }
    Ok(())
}

pub fn prepare_text_page(page: &PdfPage) -> Result<(), Box<dyn Error>> {
    // 旧解析器通过字体 ToUnicode 读取基础字形。PDFium 默认优先采用无障碍替代文本。
    // 在加载文本页前恢复字体映射；转换引擎只读取输入文档，不保存这份内存页。
    for object in page.objects().iter() {
        use_font_mapping(&object)?;
    }
    Ok(())
}
