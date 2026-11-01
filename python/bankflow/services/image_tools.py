"""本地图片转换与 PDF 互转；保留源文件，完整生成后再发布。"""
import os
import shutil
import tempfile
from contextlib import ExitStack
from pathlib import Path
from PIL import Image, ImageOps
import pypdfium2 as pdfium

FORMATS = {"png": "PNG", "jpg": "JPEG", "webp": "WEBP", "bmp": "BMP", "tiff": "TIFF"}


def opaque(image):
    rgba = image.convert("RGBA")
    background = Image.new("RGB", image.size, "white")
    background.paste(rgba, mask=rgba.getchannel("A"))
    rgba.close()
    return background


def run(request, cancelled, emit):
    mode = request["mode"]
    sources = [Path(p).resolve(strict=True) for p in (request.get("inputs") or [request["input"]])]
    if not sources or any(not p.is_file() for p in sources):
        raise ValueError("请选择存在的文件")
    fmt = request.get("format", "png")
    width = request.get("width", 0)
    quality = request.get("quality", 90)
    if fmt not in FORMATS or not isinstance(width, int) or not 0 <= width <= 16000 or not isinstance(quality, int) or not 1 <= quality <= 100:
        raise ValueError("格式、宽度或质量参数无效")
    if mode == "pdf_images" and (len(sources) != 1 or fmt not in ("png", "jpg")):
        raise ValueError("PDF 转图片请选择一个 PDF，格式为 PNG 或 JPG")
    parent = Path(request.get("output_dir") or sources[0].parent).resolve(strict=True)
    staging = Path(tempfile.mkdtemp(prefix="bankflow-", dir=parent))
    directory = mode == "pdf_images" or (mode == "image_convert" and len(sources) > 1)
    suffix = "" if directory else (".pdf" if mode == "images_pdf" else "."+fmt)
    base = sources[0].stem + {"image_convert": "_转换", "images_pdf": "_图片合并", "pdf_images": "_页面图片"}[mode]
    try:
        with ExitStack() as stack:
            pictures = []
            if mode == "pdf_images":
                doc = stack.enter_context(pdfium.PdfDocument(str(sources[0]), password=request.get("password") or None))
                total = len(doc)
                items = range(total)
            else:
                total = len(sources)
                items = sources
            if not total:
                raise ValueError("没有可转换内容")
            for index, item in enumerate(items):
                if cancelled.is_set():
                    emit("cancelled", message="任务已取消，未保存结果。")
                    return
                with ExitStack() as page_stack:
                    if mode == "pdf_images":
                        page = doc[index]; page_stack.callback(page.close)
                        w, h = page.get_size()
                        scale = width/w if width else 2
                        if w*h*scale*scale > 40000000:
                            raise ValueError("渲染图片过大，请调小输出宽度")
                        bitmap = page.render(scale=scale);page_stack.callback(bitmap.close)
                        image = bitmap.to_pil();page_stack.callback(image.close)
                    else:
                        original = page_stack.enter_context(Image.open(item))
                        if getattr(original, "n_frames", 1) != 1:
                            raise ValueError("暂不支持多帧图片，请先拆分帧，避免丢失内容")
                        image = ImageOps.exif_transpose(original);page_stack.callback(image.close)
                        if width and width != image.width:
                            height = max(1, round(image.height*width/image.width))
                            if width*height > 40000000:
                                raise ValueError("输出图片过大，请调小宽度")
                            image = image.resize((width,height),Image.Resampling.LANCZOS);page_stack.callback(image.close)
                    if mode == "images_pdf":
                        picture = opaque(image);stack.callback(picture.close);pictures.append(picture)
                    else:
                        if fmt in ("jpg", "bmp"):
                            image = opaque(image);page_stack.callback(image.close)
                        filename = f"{index+1:03d}.{fmt}" if directory else "result"+suffix
                        image.save(staging/filename, format=FORMATS[fmt], **({"quality":quality} if fmt in ("jpg","webp") else {}))
                emit("progress",page=index+1,total=total)
            if mode == "images_pdf":
                pictures[0].save(staging/"result.pdf",format="PDF",save_all=True,append_images=pictures[1:],resolution=144)
        if cancelled.is_set():
            emit("cancelled",message="任务已取消，未保存结果。")
            return
        index=0
        while True:
            output=parent/(base+(f" ({index})" if index else "")+suffix)
            try:
                if output.exists():raise FileExistsError()
                os.rename(staging if directory else staging/("result"+suffix),output)
                break
            except FileExistsError:index+=1
        emit("completed",output=str(output))
    finally:
        if staging.exists():shutil.rmtree(staging)
