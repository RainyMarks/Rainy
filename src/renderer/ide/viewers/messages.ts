/** Chinese and English strings of the file viewers: images, Markdown, PDF, audio, video, hex and decoded text. */
import { defineMessages } from '../../i18n.ts'

const zh = {
  viewSource: '源码', viewPreview: '预览', viewSplit: '分栏', viewHex: '十六进制',
  viewSourceTitle: '显示源码', viewPreviewTitle: '打开预览（Ctrl+Shift+V）', viewSplitTitle: '在侧边预览（Ctrl+K V）',
  viewHexTitle: '以十六进制查看',
  copyFailed: '无法写入剪贴板',
  loading: '正在加载…', loadFailed: '无法加载此文件：{error}', changed: '文件已在磁盘上变化，正在重新读取…',
  zoomIn: '放大', zoomOut: '缩小', zoomFit: '适应窗口', zoomAuto: '自动缩放', zoomActual: '实际大小（1:1）', zoomLevel: '{percent}%',
  imageSize: '{width} × {height}', imagePixel: '像素 ({x}, {y})', imageAttach: '添加到对话', imageAttachTitle: '把这张图片添加到 AI 输入框',
  imageAttached: '已添加到 AI 输入框', imageAttachFailed: '无法添加图片：{error}', imageChannel: '通道',
  channelOriginal: '原图', channelRed: '红色通道', channelGreen: '绿色通道', channelBlue: '蓝色通道', channelAlpha: '透明通道',
  channelBit: '{channel} 第 {bit} 位', channelInvert: '反色', channelGray: '灰度', channelPrevious: '上一个通道', channelNext: '下一个通道', channelTooLarge: '图片过大，通道视图只支持 5000 万像素以内',
  imageBroken: '无法显示这张图片，可能格式不受支持或文件已损坏',
  mediaDuration: '时长 {duration}', mediaUnsupported: '无法播放此文件，可能编码不受支持',
  pdfPage: '第 {page} / {pages} 页', pdfText: '提取文本', pdfTextTitle: '复制全部页面中的文字，包括隐藏文字',
  pdfTextCopied: '已复制 {count} 个字符', pdfFailed: '无法打开此 PDF：{error}',
  hexOffset: '偏移', hexGoto: '跳转到偏移', hexGotoPlaceholder: '如 0x1F0 或 496', hexGotoInvalid: '请输入有效偏移（十六进制加 0x 前缀）',
  hexSelection: '选中 {count} 字节，起点 0x{start}', hexNoSelection: '点击字节查看数值', hexCopy: '复制',
  hexCopyHex: '复制为十六进制', hexCopyText: '复制为文本', hexCopyPython: '复制为 Python bytes', hexCopyC: '复制为 C 数组',
  hexCopyBase64: '复制为 Base64', hexCopied: '已复制', hexCopyLimit: '一次最多复制 1 MB',
  hexInspector: '数据检查器', hexLittle: '小端', hexBig: '大端', hexType: '类型', hexUnknownType: '未识别的格式',
  hexTrailing: '{format} 结束后还有 {bytes} 字节附加数据（从 0x{offset} 开始）', hexJump: '跳转',
  hexScan: '扫描内嵌文件', hexScanTitle: '在整个文件中查找 PNG、ZIP、ELF 等文件头，类似 binwalk',
  hexScanning: '正在扫描… {percent}%', hexScanStop: '停止', hexScanNone: '没有找到内嵌文件头', hexScanFound: '找到 {count} 处文件头',
  hexScanLimit: '只显示前 {count} 处', hexOpenText: '以文本打开', hexOpenTextTitle: '按所选编码把文件当作文本只读查看',
  textDecoded: '此文件不是 UTF-8，正在按 {encoding} 只读显示；编辑仍只支持 UTF-8', textEncoding: '编码',
  textReplaced: '有 {count} 处字节无法按此编码解码', textTooLarge: '文件超过 {limit}，无法按其他编码显示',
  previewRefresh: '刷新', previewRefreshTitle: '重新读取预览中引用的图片', previewLarge: '文件较大，预览已暂停实时刷新',
  readOnlyLarge: '文件超过编辑大小限制，下面是开头部分', viewAsHex: '以十六进制查看全部',
}

const en: typeof zh = {
  viewSource: 'Source', viewPreview: 'Preview', viewSplit: 'Split', viewHex: 'Hex',
  viewSourceTitle: 'Show source', viewPreviewTitle: 'Open preview (Ctrl+Shift+V)', viewSplitTitle: 'Open preview to the side (Ctrl+K V)',
  viewHexTitle: 'View as hex',
  copyFailed: 'Could not write to the clipboard',
  loading: 'Loading…', loadFailed: 'This file cannot be loaded: {error}', changed: 'The file changed on disk. Reading it again…',
  zoomIn: 'Zoom in', zoomOut: 'Zoom out', zoomFit: 'Fit to window', zoomAuto: 'Automatic zoom', zoomActual: 'Actual size (1:1)', zoomLevel: '{percent}%',
  imageSize: '{width} × {height}', imagePixel: 'Pixel ({x}, {y})', imageAttach: 'Add to chat', imageAttachTitle: 'Add this image to the AI composer',
  imageAttached: 'Added to the AI composer', imageAttachFailed: 'The image cannot be added: {error}', imageChannel: 'Channel',
  channelOriginal: 'Original', channelRed: 'Red channel', channelGreen: 'Green channel', channelBlue: 'Blue channel', channelAlpha: 'Alpha channel',
  channelBit: '{channel} bit {bit}', channelInvert: 'Inverted', channelGray: 'Grayscale', channelPrevious: 'Previous channel', channelNext: 'Next channel', channelTooLarge: 'Channel views support images up to 50 megapixels',
  imageBroken: 'This image cannot be shown. The format may be unsupported or the file damaged.',
  mediaDuration: 'Duration {duration}', mediaUnsupported: 'This file cannot be played. Its codec may be unsupported.',
  pdfPage: 'Page {page} of {pages}', pdfText: 'Extract text', pdfTextTitle: 'Copy the text of every page, including hidden text',
  pdfTextCopied: 'Copied {count} characters', pdfFailed: 'This PDF cannot be opened: {error}',
  hexOffset: 'Offset', hexGoto: 'Go to offset', hexGotoPlaceholder: 'e.g. 0x1F0 or 496', hexGotoInvalid: 'Enter a valid offset (hex needs a 0x prefix)',
  hexSelection: '{count} bytes selected from 0x{start}', hexNoSelection: 'Click a byte to inspect it', hexCopy: 'Copy',
  hexCopyHex: 'Copy as hex', hexCopyText: 'Copy as text', hexCopyPython: 'Copy as Python bytes', hexCopyC: 'Copy as C array',
  hexCopyBase64: 'Copy as Base64', hexCopied: 'Copied', hexCopyLimit: 'Copy at most 1 MB at a time',
  hexInspector: 'Data inspector', hexLittle: 'Little-endian', hexBig: 'Big-endian', hexType: 'Type', hexUnknownType: 'Unrecognized format',
  hexTrailing: '{bytes} bytes follow the end of the {format} (from 0x{offset})', hexJump: 'Go',
  hexScan: 'Scan for embedded files', hexScanTitle: 'Search the whole file for PNG, ZIP, ELF and other headers, like binwalk',
  hexScanning: 'Scanning… {percent}%', hexScanStop: 'Stop', hexScanNone: 'No embedded file headers found', hexScanFound: '{count} file headers found',
  hexScanLimit: 'Showing the first {count}', hexOpenText: 'Open as text', hexOpenTextTitle: 'View the file as read-only text in the chosen encoding',
  textDecoded: 'This file is not UTF-8. It is shown read-only as {encoding}; editing supports UTF-8 only.', textEncoding: 'Encoding',
  textReplaced: '{count} byte sequences cannot be decoded in this encoding', textTooLarge: 'The file exceeds {limit} and cannot be shown in another encoding',
  previewRefresh: 'Refresh', previewRefreshTitle: 'Reload the images this preview shows', previewLarge: 'This file is large; the preview no longer follows every keystroke',
  readOnlyLarge: 'The file exceeds the editing limit. Its beginning is shown below.', viewAsHex: 'View all as hex',
}

/** Viewer strings. */
export const viewerMessages = defineMessages(zh, en)
/** @returns The viewer translate function for the current locale. */
export const useViewerT = viewerMessages.useT
