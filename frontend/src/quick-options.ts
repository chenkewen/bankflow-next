import type { ConversionRequest } from './api';
export const ocrImageExtensions = ['png', 'jpg', 'jpeg', 'webp', 'bmp', 'tif', 'tiff'];
export const imageInputExtensions = [
  ...ocrImageExtensions, 'gif', 'avif', 'psd', 'psb',
  'sgi', 'rgb', 'rgba', 'bw', 'pcx', 'dib', 'qoi', 'jp2', 'j2k', 'jpc', 'xbm', 'xpm', 'ras', 'sun',
  'pnm', 'pbm', 'pgm', 'ppm', 'pam', 'ff', 'ico', 'dds', 'tga', 'im',
  'msp', 'gbr', 'pxr', 'dcx', 'fit', 'fits', 'spider', 'pcd', 'imt', 'icns', 'cur', 'wmf', 'emf',
  'blp', 'ftu', 'ftc', 'fli', 'flc',
];
export const imageOutputFormats = [
  { format: 'png', description: '保留透明' },
  { format: 'jpg', description: '白色背景' },
  { format: 'webp', description: '体积与画质' },
  { format: 'bmp', description: '白色背景' },
  { format: 'tiff', description: '无损存储' },
];
export type RingProfile = 'bank' | 'general';
export interface QuickOption { quality?: number; width?: number; angle?: number; label: string; mode: ConversionRequest['mode']; format?: string; separate?: boolean }
export function quickOptions(paths: string[]): QuickOption[] {
  if (!paths.length) return [];
  const extensions = paths.map(path => path.split('.').pop()!.toLowerCase());
  if (extensions.every(ext => imageInputExtensions.includes(ext))) {
    const options: QuickOption[] = imageOutputFormats.map(({format}) => ({label:format.toUpperCase(),mode:'image_convert',format}));
    options.push({label:'PDF',mode:'images_pdf'});
    if (extensions.every(ext => ocrImageExtensions.includes(ext))) {
      options.unshift({label:paths.length > 1 ? '分别 OCR' : 'OCR 表格',mode:'ocr',separate:paths.length > 1});
    }
    return options;
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
