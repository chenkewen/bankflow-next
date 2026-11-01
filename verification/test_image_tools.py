"""检查图片尺寸、透明背景、PDF 页数和失败清理。"""
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from PIL import Image
import pypdfium2 as pdfium
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/"python"))
from bankflow.services.image_tools import run

class ImageToolsTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.source=self.root/"透明.png"
        with Image.new("RGBA",(100,50),(255,0,0,0)) as image:image.save(self.source)
        self.events=[];self.cancel=threading.Event()
    def tearDown(self):self.temp.cleanup()
    def execute(self,mode,**options):
        request=dict(mode=mode,input=str(self.source),format="png",width=0,quality=90)
        request.update(options)
        run(request,self.cancel,lambda kind,**data:self.events.append(dict(kind=kind,**data)))
        return Path(self.events[-1]["output"]) if self.events[-1]["kind"]=="completed" else None
    def test_resize_alpha_and_white_background(self):
        for fmt in ("png","jpg","webp","bmp","tiff"):
            path=self.execute("image_convert",format=fmt,width=40)
            with Image.open(path) as image:
                self.assertEqual(image.size,(40,20))
                if fmt in ("jpg","bmp"):self.assertEqual(image.convert("RGB").getpixel((0,0)),(255,255,255))
                else:self.assertEqual(image.convert("RGBA").getpixel((0,0))[3],0)
    def test_pdf_roundtrip_and_collision(self):
        path=self.execute("images_pdf",inputs=[str(self.source)]*2)
        with pdfium.PdfDocument(str(path)) as doc:self.assertEqual(len(doc),2)
        folder=self.execute("pdf_images",input=str(path),width=200)
        self.assertEqual(len(list(folder.glob("*.png"))),2)
        with Image.open(folder/"001.png") as image:self.assertEqual(image.size,(200,100))
        other=self.execute("images_pdf")
        self.assertNotEqual(path,other)
    def test_cancel_and_invalid_size(self):
        with self.assertRaises(ValueError):self.execute("image_convert",width=20000)
        self.cancel.set();self.assertIsNone(self.execute("image_convert"))
        self.assertEqual(list(self.root.iterdir()),[self.source])
