// Provider identifiers used by the transform layer (registry keys / wire types).
export const OPEN_AI: string = 'openai';
export const ANTHROPIC: string = 'anthropic';

// File-extension → MIME type map, used when shaping Anthropic file/image
// content blocks.
export const fileExtensionMimeTypeMap = {
  mp4: 'video/mp4',
  jpeg: 'image/jpeg',
  jpg: 'image/jpeg',
  png: 'image/png',
  bmp: 'image/bmp',
  tiff: 'image/tiff',
  webp: 'image/webp',
  pdf: 'application/pdf',
  csv: 'text/csv',
  doc: 'application/msword',
  docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  xls: 'application/vnd.ms-excel',
  xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  html: 'text/html',
  md: 'text/markdown',
  mp3: 'audio/mp3',
  wav: 'audio/wav',
  txt: 'text/plain',
  mov: 'video/mov',
  mpeg: 'video/mpeg',
  mpg: 'video/mpg',
  avi: 'video/avi',
  wmv: 'video/wmv',
  mpegps: 'video/mpegps',
  flv: 'video/flv',
  webm: 'video/webm',
};
