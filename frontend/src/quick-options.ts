import type { ConversionRequest } from './api';
export type RingProfile = 'bank' | 'general';
export interface QuickOption { quality?: number; width?: number; angle?: number; label: string; mode: ConversionRequest['mode']; format?: string; separate?: boolean }
export function quickOptions(paths: string[]): QuickOption[] {
  if (!paths.length) return [];
  const extensions = paths.map(path => path.split('.').pop()!.toLowerCase());
  if (extensions.every(ext => ['png','jpg','jpeg','webp','bmp','tif','tiff'].includes(ext))) {
    return [...['png','jpg','webp','bmp','tiff'].map(format => ({label:format.toUpperCase(),mode:'image_convert' as const,format})),{label:'PDF',mode:'images_pdf'}];
  }
  if (extensions.every(ext => ext === 'pdf')) {
    if (paths.length > 1) return [
      {label:'分别转 Excel',mode:'pdf',separate:true},
      {label:'分别 OCR',mode:'ocr',separate:true},
      {label:'分别转 PNG',mode:'pdf_images',format:'png',separate:true},
      {label:'分别转 JPG',mode:'pdf_images',format:'jpg',separate:true},
      {label:'分别旋转 90°',mode:'pdf_rotate',separate:true},
      {label:'合并 PDF',mode:'pdf_merge'},
    ];
    return [{label:'Excel',mode:'pdf'},{label:'OCR 表格',mode:'ocr'},{label:'PNG',mode:'pdf_images',format:'png'},{label:'JPG',mode:'pdf_images',format:'jpg'},{label:'逐页拆分',mode:'pdf_split'},{label:'旋转 90°',mode:'pdf_rotate'}];
  }
  return [];
}
