(function attachSizqoI18n(root) {
  'use strict';

  const PREFERENCE_KEY = 'sizqo-language';
  const ENGLISH = Object.freeze({
    '还原默认设置': 'Restore defaults', '历史记录': 'History', '设置': 'Settings', '切换主题': 'Switch theme', '关于': 'About',
    '外观': 'Appearance', '主题': 'Theme',
    '文件队列': 'File queue', '0 个文件': '0 files', ' 个文件': ' files', '清空列表': 'Clear list', '清除全部': 'Clear all',
    '原始大小': 'Original size', '压缩后大小': 'Compressed size', '压缩后': 'Compressed', '已节省': 'Saved', '压缩率': 'Compression ratio', '体积增加': 'size increase',
    '只显示失败': 'Show failures only', '排序': 'Sort by', '导入顺序': 'Import order', '名称': 'Name', '压缩比': 'Compression ratio', '状态': 'Status',
    '当前升序，点击切换降序': 'Currently ascending; click to sort descending', '当前降序，点击切换升序': 'Currently descending; click to sort ascending',
    '升序': 'Ascending', '降序': 'Descending', '没有失败的文件': 'No failed files', '追加文件': 'Add files', '恢复全部原图': 'Restore all originals', '继续': 'Continue',
    '开始压缩': 'Start compression', '暂停': 'Pause', '停止': 'Stop', '停止：正在处理的文件会先完成，其余文件保留在队列中': 'Stop: current files will finish; the rest stay in the queue',
    '压缩设置': 'Compression settings', '处理方式': 'Processing mode', '系统转换': 'System conversion', '高级压缩': 'Advanced compression',
    '切换处理方式，当前为': 'Switch processing mode; current mode: ', '自动处理': 'Automatic processing', '拖入或选择后自动压缩': 'Automatically compress after adding files',
    '拖入或选择后自动转换': 'Automatically convert after adding files', '格式': 'Format', '原格式': 'Original format', '图像大小': 'Image size',
    '实际大小': 'Actual size', '大（最长边 1280 px）': 'Large (longest edge 1280 px)', '中（最长边 640 px）': 'Medium (longest edge 640 px)',
    '小（最长边 320 px）': 'Small (longest edge 320 px)', '元数据': 'Metadata', '保留元数据': 'Preserve metadata', '压缩质量': 'Compression quality',
    '输出模式': 'Output mode', '覆盖原文件': 'Replace original', '添加自定义后缀': 'Add a custom suffix', '输出到指定文件夹': 'Save to a folder',
    '文件名后缀': 'Filename suffix', '例如 _small': 'For example, _small', '输出目录': 'Output folder', '未选择': 'Not selected', '浏览': 'Browse',
    '智能模式': 'Smart mode', '自动选择最佳算法和参数': 'Automatically choose the best algorithm and settings', '转换为 WebP': 'Convert to WebP',
    '输出为 WebP 格式（更小体积）': 'Use WebP output for smaller files', '输出格式': 'Output format', '保持原格式': 'Keep original format',
    '压缩引擎': 'Compression engine', '自动选择（推荐）': 'Automatic (recommended)', '现代引擎（推荐）': 'Modern engine (recommended)',
    'CLI 工具': 'CLI tools', '压缩力度': 'Compression effort', '快速': 'Fast', '平衡（推荐）': 'Balanced (recommended)', '高质量': 'High quality',
    '极致压缩（慢）': 'Maximum compression (slow)', '压缩结果': 'Compression results', '清空': 'Clear', '全部导出': 'Export all',
    '共压缩': 'Compressed', '个文件，节省': ' files, saving', '返回': 'Back', '清空历史记录': 'Clear history', '清空历史': 'Clear history',
    '还没有压缩记录': 'No compression history yet', '历史记录与原图': 'History and originals', '原图备份保留时间': 'Keep original backups for',
    '不保留': 'Do not keep', '1 天': '1 day', '3 天': '3 days', '7 天': '7 days', '14 天': '14 days', '30 天': '30 days',
    '覆盖原文件时，Sizqo 会先保存一份原图备份。': 'Sizqo saves a backup of the original before replacing a file.',
    '本次运行的压缩记录和原图备份都会在关闭应用时清理；期间可以随时恢复原图。': 'Compression history and original backups from this session are cleared when the app closes. You can restore originals before then.',
    '性能': 'Performance', '当前设备': 'This device', '检测中…': 'Detecting…', 'CPU 使用上限': 'CPU usage limit',
    'Sizqo 最多使用的 CPU 并行份数': 'Maximum number of CPU workers used by Sizqo', '自动': 'Automatic',
    '不指定，按机器性能自动决定': 'Let Sizqo choose based on this device', '低占用': 'Lower usage', '高性能': 'Higher performance',
    '限制 Sizqo 同时使用的 CPU 并行能力。': 'Limit how much CPU parallelism Sizqo can use.',
    '较低的数值会降低压缩速度，但可为其他应用保留更多性能。': 'A lower setting reduces compression speed and leaves more resources for other apps.',
    '系统会自动在性能核与能效核之间调度任务。': 'The system schedules work across performance and efficiency cores.',
    '更新': 'Updates', '当前版本': 'Current version', '在线更新': 'App updates', '检查更新': 'Check for updates',
    '打开 GitHub Release 页面': 'Open the GitHub Releases page', 'Release 页面': 'GitHub Releases', '下载进度': 'Download progress',
    '下载中 0%': 'Downloading 0%', '取消': 'Cancel',
    '检查更新只访问 GitHub Releases；发现新版本要点「立即更新」才会下载，图片始终不离开这台机器。': 'Update checks only contact GitHub Releases. A download starts only when you select Update now; your images never leave this device.',
    '本版本由 Mac App Store 分发，更新由 App Store 统一管理，应用内不再检查版本。': 'This version is distributed through the Mac App Store. Updates are managed by the App Store.',
    '系统转换说明': 'About system conversion', '关闭': 'Close',
    '系统转换使用 macOS 自带的图像转换能力，行为对齐 Finder 中选中图片后右键「快速操作 → 转换图像」的默认方式。': 'System conversion uses macOS built-in image conversion, similar to Finder’s Quick Actions > Convert Image.',
    '可选择输出格式、图像大小和是否保留元数据；不会调用第三方压缩工具。此模式仅在 macOS 上可用。': 'Choose an output format, image size, and whether to preserve metadata. No third-party compression tools are used. This mode is available only on macOS.',
    '知道了': 'Got it', '原图对比': 'Image comparison', '上一个（⌘+滚轮）': 'Previous (⌘ + scroll)', '上一个': 'Previous',
    '下一个（⌘+滚轮）': 'Next (⌘ + scroll)', '下一个': 'Next', '缩小': 'Zoom out', '放大': 'Zoom in', '重置为适合窗口': 'Fit to window',
    '加载中…': 'Loading…', '原图': 'Original', '压缩后': 'Compressed', '文件名': 'Filename', '原图大小': 'Original size', '节省': 'Saved',
    '算法': 'Algorithm', '重新压缩质量': 'Recompress quality', '重新压缩': 'Recompress', '恢复原图': 'Restore original',
    '自动（跟随系统）': 'Follow system', '简体中文': 'Simplified Chinese', '英语': 'English', '界面语言': 'Interface language', '语言': 'Language',
    '默认跟随系统语言；更改后界面会重新加载。': 'Follows your system language by default. The interface reloads when you change this setting.',
    '系统': 'System', '智能': 'Smart', '标准': 'Standard', '覆盖': 'Replace', '后缀': 'Suffix', '目录': 'Folder',
    '原图对比': 'Image comparison',
    '亮色模式': 'Light mode', '暗黑模式': 'Dark mode', '当前: ': 'Current: ', ' · 点击切换': ' · click to switch', '亮色': 'Light', '暗黑': 'Dark', '主题: ': 'Theme: ',
    '转换中…': 'Converting…', '压缩中…': 'Compressing…', '转换完成': 'Conversion complete', '压缩完成': 'Compression complete',
    '继续转换': 'Continue conversion', '继续压缩': 'Continue compression', '开始转换': 'Start conversion', '开始压缩': 'Start compression',
    '选择图片失败，请重试': 'Could not select images. Please try again.', '选择文件夹失败，请重试': 'Could not select a folder. Please try again.',
    '请选择系统转换文件的输出文件夹': 'Choose an output folder for system conversion.', '无法读取拖入文件，请使用“选择文件”': 'Could not read the dropped files. Use “Choose Files” instead.',
    '读取图片失败，请重试': 'Could not read the images. Please try again.', '文件夹中没有找到可压缩的图片': 'No supported images found in the folder.',
    '所选图片已在队列中': 'The selected images are already in the queue.', '添加图片失败，请重试': 'Could not add images. Please try again.',
    '已跳过': 'Skipped', '只处理这几个': 'Process selected files only', '处理过': 'Processed', '已处理': ' processed', ' · 失败 ': ' · failed ',
    // §42 重试说明：分子不退，只补一句「正在重试 N 项」。
    ' · 正在重试 ': ' · retrying ', ' 项': ' item(s)',
    ' · 已暂停': ' · paused', ' · 正在停止': ' · stopping', '还有几个在跑': 'Some files are still processing', '上限 / 可用并行数': 'Limit / available workers',
    '同时允许几份 CPU 并行压缩工作': 'Maximum concurrent CPU compression tasks', ' · CPU 自动': ' · CPU automatic', '失败': 'Failed', '已恢复': 'Restored', '已移除': 'Removed',
    '等待中': 'Waiting', '已暂停': 'Paused', '移除': 'Remove', '删除这次压缩结果': 'Delete compressed file', '另存为': 'Save as', '对比查看': 'Compare',
    '在访达中显示': 'Show in Finder', '重试': 'Retry', '复制日志': 'Copy log', '版本: ': 'Version: ', '=== Sizqo 压缩日志 ===': '=== Sizqo compression log ===',
    '=== Sizqo 压缩历史 ===': '=== Sizqo compression history ===', '文件: ': 'File: ', '时间: ': 'Time: ', '状态: ': 'Status: ', '成功': 'Succeeded',
    '--- 压缩参数 ---': '--- Compression settings ---', '(未设置)': '(not set)', '--- 压缩结果 ---': '--- Compression result ---', '--- 原图备份 ---': '--- Original backup ---', '未知错误': 'Unknown error', '原始大小: ': 'Original size: ',
    '压缩后大小: ': 'Compressed size: ', '压缩率: ': 'Compression ratio: ', '输出格式: ': 'Output format: ', '(未知)': '(unknown)', '算法: ': 'Algorithm: ',
    '压缩失败': 'Compression failed', '--- 错误信息 ---': '--- Error details ---', '(无)': '(none)', '压缩日志已复制到剪贴板': 'Compression log copied to clipboard.',
    '复制失败，请手动选中日志文本': 'Could not copy. Select and copy the log manually.', '请先选择输出目录': 'Choose an output folder first.', '确定要把 ': 'Remove ',
    ' 个文件移出清单吗？磁盘上的原图和压缩产物都不会删除。': ' files from the list? The originals and the compressed files on disk are not deleted.', '无法确认输出目录，请重试': 'Could not verify the output folder. Please try again.', '已有一轮压缩正在运行，请等它结束或先停止': 'Another compression session is already running; wait for it to finish or stop it first', '转换出错: ': 'Conversion error: ', '压缩出错: ': 'Compression error: ',
    '已重新压缩: ': 'Recompressed: ', '无法保存：找不到压缩文件': 'Could not save: compressed file not found.', '已保存到: ': 'Saved to: ',
    '这个文件在压缩后又被修改过。\n恢复原图会覆盖当前版本。': 'This file was changed after compression.\nRestoring the original will overwrite the current version.',
    '恢复失败: ': 'Restore failed: ', '恢复失败': 'Restore failed', '已恢复原图: ': 'Original restored: ', '已删除这次压缩结果: ': 'Deleted compressed file: ',
    '确定要恢复全部已压缩成功的原图吗？': 'Restore all originals that were compressed successfully?', '已导出 ': 'Exported ', ' 个文件到原目录（': ' files to the original folder (', ' 后缀）': ' suffix)',
    '正在停止…': 'Stopping…', '收尾中…': 'Finishing…', ' 暂停中…': ' Pausing…', '继续压缩剩余文件': 'Continue compressing remaining files',
    '暂停：不再启动新文件': 'Pause: do not start new files', '停止：正在处理的文件会先完成，其余文件保留在队列中': 'Stop: current files will finish; the rest stay in the queue',
    ' 处理完成 · ': ' completed · ', ' 个失败': ' failed', '暂停失败，请重试': 'Could not pause. Please try again.', '继续失败，请重试': 'Could not resume. Please try again.',
    '正在停止：正在处理的文件会先完成，其余文件保留在队列中': 'Stopping: current files will finish; the rest stay in the queue', '停止失败，请重试': 'Could not stop. Please try again.',
    '今天 ': 'Today ', '昨天 ': 'Yesterday ', '检测到可恢复的原图备份': 'A recoverable original backup was found', '原图备份已清理': 'Original backup was cleaned up',
    '原图未覆盖': 'Original was not overwritten', '原文件位置不存在': 'Original file location does not exist', '已压缩': 'Compressed', '压缩结果已不存在': 'Compressed file no longer exists',
    '输出文件不存在': 'Output file does not exist', '记录不完整 · 原图备份已清理': 'Incomplete record · original backup was cleaned up',
    '压缩结果未能删除': 'The compressed output could not be deleted',
    // §52 §78 §79 磁盘复核失败与批量摘要的片段（前端按计数拼，后端那句中文只进日志）。
    '文件不存在或已被移动': 'The file does not exist or has been moved', '保存失败: ': 'Could not save: ',
    '导出失败: ': 'Export failed: ', '在访达中显示失败: ': 'Could not reveal in Finder: ',
    '之前已经完成过这一步，这次没有再做改动': 'This step was already completed; nothing was changed this time',
    '恢复完成 · 成功 ': 'Restore complete · succeeded: ', '导出完成 · 成功 ': 'Export complete · succeeded: ',
    ' · 记录不存在 ': ' · missing records: ', ' · 文件不存在 ': ' · missing files: ',
    ' · 已恢复但压缩产物未能删除 ': ' · restored but output could not be deleted: ',
    ' · 跳过没有产物的项 ': ' · skipped items without output: ',
    '打开对比窗口失败: ': 'Could not open comparison window: ', '删除这次压缩结果？\n将删除 ': 'Delete this compressed file?\nThis will delete ',
    '，并移除这条历史记录。原图未被覆盖，不受影响。': '. This history entry will be removed. The original was not overwritten.', '明细: 已丢失（这条记录是按原图备份重建出来的）': 'Details unavailable (this entry was rebuilt from the original backup)',
    '输出: ': 'Output: ', '输出方式: ': 'Output mode: ', '明细已丢失': 'Details unavailable', '节省 ': 'Saved ', '按备份重建': 'Rebuilt from backup', ' 条': ' entries',
    '压缩进行中，这一批结束后才能清空历史': 'Compression is in progress. Clear history after this batch finishes.', '读取历史记录失败': 'Could not read compression history.',
    '确定要清空 ': 'Clear ', ' 条历史记录吗？\n': ' history entries?\n', '同时立即删除 Sizqo 保存的 ': 'Also delete the ',
    ' 份原图备份，不必等保留期到期。不会删除你的任何图片文件。': ' original backups saved by Sizqo. Your image files will not be deleted.',
    'Sizqo 目前没有保存原图备份，这次只清记录。不会删除你的任何图片文件。': 'Sizqo has no saved original backups. Only history will be cleared; your image files will not be deleted.',
    '不会删除你的任何图片文件': 'Your image files will not be deleted', '已清空 ': 'Cleared ', ' 条历史记录': ' history entries', '清空失败: ': 'Could not clear history: ',
    '原图备份改为不保留，关闭应用时清理记录和备份': 'Original backups will not be kept. History and backups are cleared when the app closes.',
    '原图备份保留 ': 'Keep original backups for ', ' 天，关闭应用时清理过期项': ' days; expired items are cleared when the app closes.', '设置失败: ': 'Could not save settings: ',
    '过期的历史记录和原图备份将在关闭应用时自动清理。': 'Expired history and original backups are cleared automatically when the app closes.', '关掉备份': 'Disable backups', '下次启动': 'next launch',
    ' 核 CPU（': '-core CPU (', ' 性能核 + ': ' performance + ', ' 能效核）': ' efficiency cores)', ' 个物理核心 · ': ' physical cores · ', ' 个逻辑处理器': ' logical processors',
    '最多 ': 'Up to ', ' 份并行计算': ' parallel workers', '自动（': 'Automatic (', '（全部）': '(all)', 'CPU 上限改为自动（': 'CPU limit set to automatic (',
    'CPU 上限改为 ': 'CPU limit set to ', '没有可对比的结果': 'No results to compare', '打开对比失败: ': 'Could not open comparison: ',
    '检查中': 'Checking…', '立即更新': 'Update now', ' 可用': ' available', '已是最新版本': 'You are up to date', '检查失败': 'Check failed', '下载中 0%': 'Downloading 0%',
    '下载中 ': 'Downloading ', '安装中…': 'Installing…', '更新失败': 'Update failed', '已取消': 'Cancelled', '在线更新检查失败:': 'Online update check failed:',
    '暂无可对比的内容': 'Nothing to compare', '无法加载原图，文件可能已被移动或恢复': 'Could not load the original. It may have been moved or restored.',
    '无法加载压缩图，文件可能已被移动或恢复': 'Could not load the compressed image. It may have been moved or restored.', '打开对比失败: ': 'Could not open comparison: ',
    '重新压缩预览加载失败': 'Could not load the recompression preview', '重新压缩完成 (质量: ': 'Recompression complete (quality: ', '重新压缩没有产出：': 'No output from recompression: ',
    '重新压缩出错: ': 'Recompression error: ', '恢复出错: ': 'Restore error: ', '该文件已恢复原图或被移除': 'This file was restored or removed', '加载失败: ': 'Load failed: ',
    '原始大小: ': 'Original size: ', '压缩后大小 ': 'Compressed size ', '保存': 'Save',
    '按所选编码器解释；数值不能跨编码器直接比较': 'Interpreted by the selected encoder; values are not comparable across encoders',
    '预计体积是估算；实际结果取决于素材内容、编码器和轨道开销。': 'The estimated size is an estimate; the actual result depends on content, encoder, and track overhead.',
    '转换为 AAC 160 kbps': 'Convert to AAC 160 kbps', '转换为 Opus 160 kbps': 'Convert to Opus 160 kbps',
    '转换为 MP3 160 kbps': 'Convert to MP3 160 kbps', '转换为 FLAC 无损': 'Convert to FLAC (lossless)',
    'kbps · 估算不保证最终体积': 'kbps · Estimated; the final size is not guaranteed', 'MiB · 音频与容器开销会影响结果': 'MiB · Audio and container overhead affect the result',
    '恒定质量（CRF）': 'Constant quality (CRF)', '目标码率（ABR）': 'Target bitrate (ABR)', '两遍码率': 'Two-pass bitrate',
    '保持源分辨率': 'Keep source resolution', '自动（保持时长）/ VFR': 'Auto (preserve duration) / VFR', '自定义最长边像素': 'Custom longest edge in pixels',
    '最高 4K': 'Up to 4K', '最高 1440p': 'Up to 1440p', '最高 1080p': 'Up to 1080p', '最高 720p': 'Up to 720p', '自定义最长边': 'Custom longest edge',
    '兼容音轨优先复制': 'Copy compatible audio first', '移除隐私位置数据': 'Remove privacy location data',
    '移除音频': 'Remove audio', '移除字幕': 'Remove subtitles', '保留兼容字幕': 'Keep compatible subtitles', '全部移除': 'Remove all',
    '允许保留更大的输出': 'Allow keeping a larger output',
    '图片输出方式': 'Image output mode', '视频输出方式': 'Video output mode',
    '图片输出': 'Image output', '视频输出': 'Video output',
    '视频同帧对比': 'Video frame comparison', '原视频帧': 'Source video frame', '输出视频帧': 'Output video frame',
    '视频设置': 'Video settings', '视频编码器': 'Video encoder', '历史媒体类型': 'History media type',
    '原视频': 'Source video', '输出视频': 'Output video', '媒体队列': 'Media queue', '媒体类型': 'Media type',
    '输出容器': 'Output container', '输出方式': 'Output mode', '高级选项': 'Advanced options', '编码速度': 'Encoding speed', '编码线程': 'Encoding threads',
    '0 = 自动': '0 = automatic', '控制模式': 'Control mode', '质量值': 'Quality value', '结果策略': 'Result policy',
    '目标码率': 'Target bitrate', '目标体积': 'Target size', '分辨率': 'Resolution', '帧率': 'Frame rate', '预设': 'Preset',
    '全部': 'All', '图片': 'Images', '视频': 'Videos', '历史': 'History', '音频': 'Audio', '字幕': 'Subtitles', '保留': 'Keep', '缩放': 'Zoom',
    '正在检测视频编码能力…': 'Detecting video encoding support…', '视频预设不可用': 'Video presets unavailable',
    '自动（按能力选择）': 'Automatic (based on capabilities)', '（当前运行时不可用）': ' (unavailable in this runtime)',
    '（仅 Direct）': ' (Direct only)', '此版本当前不提供可运行的视频编码器。': 'This build has no runnable video encoders.',
    'Direct 使用独立 FFmpeg 编码能力；编码器按当前运行时检测结果开放。': 'Direct uses bundled FFmpeg; encoders are offered per runtime detection.',
    '视频能力探测失败': 'Video capability detection failed', '当前运行时不支持视频编码': 'Video encoding is not supported in this runtime',

    /* ── 意图模型（core/intent-model.md）：chips、说明、编译结论 ─────────────
     * 这些字符串来自 frontend/intents.js 的表，界面每一处都过 localizeUiText。 */
    '肉眼无差优先': 'Visually lossless', '均衡': 'Balanced', '尽量变小': 'As small as possible',
    '网页尺寸': 'Web size', '指定体积': 'Target size', '自定义': 'Custom', '系统转换尺寸': 'System conversion size',
    '兼容优先': 'Compatibility first', '通用更小': 'Smaller everywhere', '高画质 · 保留轨道': 'High quality · keep tracks',
    '1080p 网页': '1080p web', '极致体积': 'Smallest size', '高质量压缩': 'High quality compression',
    '质量 90，不改变像素尺寸；候选搜索挑最小，PNG 可能被换成更小的 WebP。': 'Quality 90 with pixel dimensions untouched; the candidate search keeps the smallest result, so a PNG may become a smaller WebP.',
    '质量 75，不改变像素尺寸；候选搜索挑最小，PNG 可能被换成更小的 WebP。默认。': 'Quality 75 with pixel dimensions untouched; the candidate search keeps the smallest result, so a PNG may become a smaller WebP. Default.',
    '质量 60，候选搜索取最小；PNG 会同时试 WebP 并按更小者输出，JPG 只压 JPG。': 'Quality 60; the candidate search keeps the smallest result, so a PNG may become a smaller WebP while a JPG stays a JPG.',
    '质量档偏低，细节损失看得出来；高级路径重编码像素，EXIF 与 GPS 不会带进输出。': 'A low quality rung shows visible detail loss; the advanced path re-encodes pixels, so EXIF and GPS are not carried into the output.',
    '最长边压到 2560 px 以内（只缩不放），质量 72，格式保持不变。': 'Longest edge capped at 2560 px (shrink only), quality 72, format untouched.',
    '在体积预算内自动组合尺寸与质量，取达标的最小结果；达不到就报最接近的结果，不谎报成功。': 'Sizqo combines dimensions and quality to fit the budget and keeps the smallest result that meets it; when it cannot, it reports the closest result instead of claiming success.',
    '达标与否逐文件报告；预算远低于素材下限时只能给出最接近的结果。高级路径重编码像素，EXIF 与 GPS 不会带进输出。': 'Each file reports whether it met the budget; a budget below what the source can shrink to only yields the closest valid result. The advanced path re-encodes pixels, so EXIF and GPS are not carried into the output.',
    '用 macOS 自带的图像转换，行为对齐 Finder 的「快速操作 → 转换图像」，可保留元数据属性。': 'Uses the image conversion built into macOS, matching Finder Quick Actions, and can keep metadata.',
    '高级路径重编码像素，EXIF 与 GPS 不会带进输出。': 'The advanced path re-encodes pixels, so EXIF and GPS are not carried into the output.',
    '高级路径重编码像素，EXIF 与 GPS 不会带进输出；要保留元数据请改用系统转换。': 'The advanced path re-encodes pixels, so EXIF and GPS are not carried into the output; switch to System conversion to keep metadata.',
    '会改变像素尺寸；高级路径重编码像素，EXIF 与 GPS 不会带进输出。': 'Pixel dimensions change; the advanced path re-encodes pixels, so EXIF and GPS are not carried into the output.',
    '直接用下面这些参数，意图不再介入：怎么写就怎么发，PNG 不会被换成更小的 WebP。改动任一参数即切到这里。': 'Uses the parameters below exactly as written; no intent is applied, and a PNG is never swapped for a smaller WebP behind your back. Changing any one of them lands here.',
    '直接用高级选项里的参数，意图不再介入。': 'Uses the advanced options exactly as written; no intent is applied.',
    '按目标体积反推码率并走两遍编码。结果是估算，不是保证。': 'Bitrate is derived from the target size and encoded in two passes. The result is an estimate, not a guarantee.',
    '目标体积是估算：编码内容决定最终大小，实际会有百分之几的偏差。': 'The target size is an estimate: the content decides the final size, so expect a few percent of drift.',
    '最长边不超过 1080 · CRF 26 · MP4 · AAC 160 kbps。': 'Longest edge up to 1080 · CRF 26 · MP4 · AAC 160 kbps.',
    'CRF 20，音轨与字幕原样复制，元数据保留。体积减少有限。': 'CRF 20 with audio and subtitle tracks copied and metadata kept. Expect limited savings.',
    '意图条里的其他目标走高级压缩引擎，会重编码像素，EXIF 与 GPS 不会带进输出。': 'The other intents use the advanced engine, which re-encodes pixels, so EXIF and GPS are not carried into the output.',
    '预算 ': 'Budget ', '最长边 ': 'Longest edge ', '目标 ': 'Target ', ' MiB（估算）': ' MiB (estimate)',
    '大 1280px': 'Large 1280 px', '中 640px': 'Medium 640 px', '小 320px': 'Small 320 px',
    '（估算）': ' (estimate)', '估算，非保证': 'Estimated, not guaranteed', '只缩不放': 'Shrink only',
    ' · 两遍': ' · two-pass',

    /* ── 工作台外壳：导航、运行条、详情列、历史 ─────────────────────────
     * 这些是「双栏工作台 + 意图优先」这轮新加的界面文本。 */
    '媒体工作台': 'Media workspace', '本轮': 'This round', '压缩历史': 'Compression history',
    '视图': 'View', '追加': 'Add', '清空队列': 'Clear queue',
    '图片意图': 'Image goal', '视频意图': 'Video goal', '图片目标': 'Image target', '视频目标': 'Video target',
    '更多参数': 'More parameters', '原图与备份': 'Originals and backups', '存为预设': 'Save as preset',
    '回到导入': 'Back to import', '移出清单': 'Remove from list',
    '结束这一轮，回到拖入文件那一页；磁盘上的文件和压缩产物都不动': 'End this round and go back to the import page. Files on disk and the compressed results are untouched.',
    '把这些文件移出清单，不删除磁盘上的文件': 'Remove these files from the list without deleting anything on disk',
    '已回到导入页 · 这一轮的结果在历史里': 'Back at the import page · this round stays in the history',
    '把当前高级选项存成视频预设': 'Save the current advanced options as a video preset',
    '体积预算': 'Size budget', '最长边上限': 'Longest edge cap', '加后缀': 'Add suffix', '输出到目录': 'Save to a folder',
    '图片输出方式': 'Image output mode', '视频输出方式': 'Video output mode',
    '选择左侧的文件看详情': 'Select a file on the left to see its details',
    '更轻的文件，让创作更自由': 'Lighter files, more room to create',
    '每一次压缩都留了回来的路': 'Every compression keeps a way back',
    '省 ': 'Saved ', '多 ': 'Grew ', ' 个产物': ' files', ' 后缀': ' suffix',
    '队列为空': 'Queue is empty', '未开始': 'Not started', '正在停止': 'Stopping', '已停止': 'Stopped', '结束': 'Ended',
    '已完成': 'Finished', '耗时': 'Elapsed', '体积目标': 'Size target', '是否达标': 'Met the target',
    '达标': 'Met', '未达标': 'Not met', '达标 ≤ ': 'Met ≤ ', '未达 ': 'Missed ',
    '(不限)': '(no cap)', '意图: ': 'Intent: ', '--- 这一轮的请求 ---': '--- Request for this round ---',
    '输出位置': 'Output location', '输出到 ': 'To ', '（还没选目录）': '(no folder chosen)', '同目录 · ': 'Same folder · ',
    '覆盖原文件（保留可恢复的备份）': 'Replace the original (keeps a restorable backup)',
    '尽量保留（系统转换）': 'Kept where possible (system conversion)', '高级路径重编码后不保留': 'Not preserved after advanced re-encoding',
    '原图备份': 'Original backup', '原文件': 'Original file', '时间': 'Time', '保留至': 'Kept until',
    '已不在磁盘上': 'No longer on disk', '位置已不存在': 'Location no longer exists',
    '原图从未被覆盖': 'The original was never replaced', '已清理，无法恢复原图': 'Cleaned up; the original cannot be restored',
    '明细已丢失（按原图备份重建）': 'Details lost (rebuilt from the original backup)',
    '这条记录是从备份目录重建的：这次压缩的明细已无从得知，但备份还在，原图仍能恢复。': 'This record was rebuilt from the backup folder: the details of that run are gone, but the backup is here and the original can still be restored.',
    '原图已恢复，压缩产物已按记录处理。': 'The original is back, and the output was handled as recorded.',
    '压缩结果已不在磁盘上（可能被手动删除）。': 'The compressed output is no longer on disk; it may have been deleted by hand.',
    '原图已在 ': 'Original restored at ', ' 写回。': '.',
    '选择一条记录，看它此刻在磁盘上还剩什么': 'Select a record to see what is still on disk',
    '此类别还没有压缩记录': 'No records in this category yet',
    '重试这一个': 'Retry this one', '重新压缩这一个': 'Recompress this one', '立即压缩这一个': 'Compress this one now',
    '移出队列': 'Remove from queue', '查看原因': 'See the reason', '这次压缩没有成功': 'This compression did not succeed',
    '删除这次结果': 'Delete this output',
    '有效且更小，已按默认规则采用。': 'Valid and smaller, kept by the default rule.',
    '没能在预算内达标：给出的是这一轮最小的有效结果。': 'The budget was not met; this is the smallest valid result from the round.',

    /* ── "引擎成功了，但盘上没有文件"这一类状态 ───────────────────────── */
    '未产出文件': 'Nothing was written', ' 未产出文件': ' nothing was written',
    '没有产物 · 原图未改动': 'No output · the original was left untouched',
    '这次没有写出文件：': 'Nothing was written this time: ',
    '产物不比原图小，原图保持原样。': 'The output was not smaller than the original, so the original stayed as it was.',
    '产物不比原文件小，原文件保持原样。': 'The output was not smaller than the source, so the source stayed as it was.',
    '未写出文件 · 原图保持原样': 'No file written · original left untouched',
    '产物: ': 'Output: ',

    /* ── 结果契约（ResultOutcome / ReasonCode）的界面措辞：方案 §70 ─────── */
    '已完成 · 未达到目标': 'Finished · target not met',
    '处理失败': 'Processing failed',
    '没有产物 · 原文件未改动': 'No output · the source file was left untouched',
    '原因: ': 'Reason: ',
    // reasonCode → 句子。前端只按码选词，绝不解析后端文案（§70 §86）。
    '原文件已经小于目标体积': 'The source was already below the target size',
    '压缩后没有变小': 'Compressing did not make it smaller',
    '压缩后文件更大': 'The compressed file would be larger',
    '未达到目标体积': 'The target size was not met',
    '这一项被取消，没有开始处理': 'This item was cancelled before it started',
    '已停止，未开始': 'Stopped before it started',
    '压缩结果写入失败': 'The result could not be written',
    '历史记录保存失败，已自动恢复原图': 'The history entry could not be saved, so the original was restored',
    '这个格式暂不支持': 'This format is not supported yet',
    '输入无效': 'Invalid input',
    '无法解码这个文件': 'This file could not be decoded',
    '编码失败': 'Encoding failed',
    '无法读取源文件': 'The source file could not be read',
    '源文件不存在': 'The source file does not exist',
    '没有权限访问这个文件': 'Permission to access this file was denied',
    '源文件在处理期间被改动，已保留你的版本': 'The source changed while it was being processed; your version was kept',
    '无法保存原图备份': 'The original backup could not be saved',
    '无法登记这次覆盖': 'This replacement could not be journaled',
    '自动恢复原图也没成功，备份仍在': 'Automatic restore also failed; the backup is still there',
    '目标位置已有同名文件': 'A file with that name already exists at the destination',
    '输出校验未通过': 'The output failed validation',
    '原因未上报': 'No reason was reported',
    // 后端原话会出现在悬停与详情里，英文界面下不能留成中文。
    // 这一组只会被 translateMessage（整句匹配）读到：带 {detail} 尾巴的模板句没法整句命中，
    // 就让它原样留着 —— 半译的句子比中文更难读。
    // 「引擎压成功但没落盘」那句不再由后端写成一句话上屏：它现在是 outcome+reasonCode，
    // 句子在前端按码选（见上面 §70 那一组）。
    '压缩过程发生内部错误': 'Compression stopped with an internal error',
    '压缩结果已生成，但历史记录保存失败，已自动恢复原图': 'The compressed file was produced, but the history entry could not be saved, so the original was restored automatically',
    '请先选择输出目录': 'Choose an output folder first',
    // 恢复失败的这六句是 RestoreError::message() 的全文（history.rs），Toast 直接念出来。
    '找不到这条历史记录': 'That history entry could not be found',
    '原图未被覆盖，无需恢复': 'The original was never overwritten, so there is nothing to restore',
    '原图备份已清理，无法恢复': 'The original backup was already cleaned up, so this cannot be restored',
    '这个文件在压缩后又被修改过': 'This file was modified again after compression',
    '该备份来自旧版本，缺少内容指纹': 'This backup came from an older version and has no content fingerprint',
    '检测到需要人工处理的恢复现场': 'Recovery scenes need your decision',
    '共 ': '',
    ' 个现场待处理': ' scene(s) waiting',
    '当前文件: ': 'Current file: ',
    '（存在，': ' (present, ',
    ' 字节）': ' bytes)',
    '（不存在）': ' (missing)',
    '原图备份: ': 'Original backup: ',
    '（已丢失）': ' (gone)',
    '警告：备份字节与记账时的指纹不符': 'Warning: the backup bytes no longer match the fingerprint recorded in the journal',
    '恢复原图': 'Restore original',
    '暂不处理': 'Keep the scene',
    '恢复原图会覆盖当前位置的文件：': 'Restoring the original will overwrite the file at:\n',
    '\n\n将写回的原图备份：': '\n\nOriginal backup to be written back:',
    '\n\n警告：这份备份的字节与记账时的指纹不符，恢复的内容可能不是当初的原图。继续即表示你了解并接受这一风险。': '\n\nWarning: this backup no longer matches the recorded fingerprint; what gets restored may not be the original. Continuing means you understand and accept this risk.',
    '已处理': 'Done',
    '处理失败: ': 'Failed: ',
    '现场已保留：原图备份与事务日志都在原处，随时可以再处理': 'Scene kept: the original backup and the journal stay where they are; you can handle it any time',
    '原图已恢复，事务已销账': 'Original restored; the journal is settled',
    '没有该文件夹的访问权限，请重新授权': 'Sizqo no longer has access to that folder, please grant it again',
    ' · 已恢复 ': ' · restored ',
    // §44 会话期间恢复被禁用时，行 / 详情 / 批量按钮共用这一句原因。
    '压缩进行中，暂不能恢复；请先停止或等这一轮结束': 'Compression is running, so restoring is unavailable; stop it or wait for this round to finish',

    /* ── 视频：行内读数、控制、校验 ─────────────────────────────────── */
    '时长': 'Duration', '尺寸': 'Dimensions', '编码': 'Codec', '编码器': 'Encoder', '音轨': 'Audio',
    '音轨策略': 'Audio policy', '后端': 'Backend', '无音轨': 'No audio tracks', ' 条音轨': ' audio tracks',
    '剩余 ': 'Remaining ', '预计输出约 ': 'Estimated output ', '这个素材还没有可靠的体积估算。': 'No reliable size estimate for this source yet.',
    '检测到 ': 'Detected ', '；须确认 HDR 色彩和动态元数据保真': '; HDR colour and dynamic-metadata fidelity must be confirmed',
    '已重新探测并完整解码校验后才提交。': 'It was probed again and fully decoded before it was committed.',
    '同帧对比': 'Frame comparison', '查看日志': 'View log', '播放结果': 'Play output',
    '播放原视频': 'Play source video', '播放输出视频': 'Play output video', '取消当前视频': 'Cancel current video',
    '正在取消…': 'Cancelling…', '正在读取视频信息…': 'Reading video info…', '正在解码所选时间点…': 'Decoding the selected time…',
    '恢复原视频': 'Restore the source video', '已恢复原视频: ': 'Source video restored: ',
    '原视频帧解码失败': 'Could not decode the source video frame', '输出视频帧解码失败': 'Could not decode the output video frame',
    '原视频或压缩结果已不存在': 'The source video or its output no longer exists',
    '当前视频已取消，输入文件仍留在队列中': 'The current video was cancelled; the file stays in the queue',
    '视频日志已复制到剪贴板': 'Video log copied to the clipboard',
    '无法打开媒体文件: ': 'Could not open the media file: ',
    '无法选择视频输出目录: ': 'Could not choose the video output folder: ',
    '无法读取两段视频的有效时长': 'Could not read a valid duration from both videos',
    '无法生成同帧预览': 'Could not generate the frame preview',
    '媒体分析失败，请重试': 'Media analysis failed; please try again', '添加媒体失败，请重试': 'Could not add media; please try again',
    '所选媒体已在队列中': 'Those files are already in the queue', '没有找到可处理的图片或视频': 'Nothing here can be compressed',
    '此版本当前不支持视频编码': 'This build does not support video encoding yet',
    '视频编码不可用；本轮仅处理可运行的图片任务': 'Video encoding is unavailable; this round only runs image work',
    '视频编码当前不可用': 'Video encoding is unavailable right now', '视频编码能力已检测。': 'Video encoding capability detected.',
    '视频参数校验失败': 'Video settings failed validation', '视频设置无效': 'Video settings are invalid',
    '该视频与当前设置不兼容': 'This video is incompatible with the current settings',
    '所选视频预设当前不可用': 'The selected video preset is unavailable right now',
    '所选编码器当前不可用': 'The selected encoder is unavailable right now',
    '所选编码器不支持此控制模式': 'The selected encoder does not support this control mode',
    '所选 H.264 编码器不能保留此 10-bit 视频格式': 'The selected H.264 encoder cannot keep this 10-bit format',
    '将按当前编码器的 HDR 保留能力处理色彩与动态元数据': 'Colour and dynamic metadata follow the selected encoder HDR retention',
    '检测到 HDR / Dolby Vision；当前能力未确认可保留色彩与动态元数据，已阻止转码': 'HDR / Dolby Vision detected; keeping colour and dynamic metadata is not confirmed for this runtime, so transcoding was blocked',
    '已允许保留大于原件的输出文件': 'Outputs larger than the source are allowed to stay',
    '未知分辨率': 'Unknown resolution', '未知编码': 'Unknown codec', '自定义目标大小': 'Custom target size',
    '现在真正在跑什么': 'What is actually running now',
    'FLAC 音轨需要 MKV 容器': 'FLAC audio needs an MKV container',
    'MP4 不支持 Opus 音轨；请选择 AAC 或 MKV': 'MP4 does not support Opus audio; choose AAC or MKV',
    'TS 不支持 Opus 音轨；请选择 AAC/MP3 或 MKV': 'TS does not support Opus audio; choose AAC/MP3 or MKV',
    'WebM 不能使用 H.264 或 HEVC 编码': 'WebM cannot use H.264 or HEVC',
    '源音频与 WebM 不兼容；后端需转换或拒绝此组合': 'The source audio does not fit WebM; the backend must convert it or reject this combination',
    '输出容器不受支持': 'Output container is not supported', '输出方式不受支持': 'Output mode is not supported',
    '音频策略不受支持': 'Audio policy is not supported', '字幕策略不受支持': 'Subtitle policy is not supported',
    '音轨复制': 'Audio copied',
    '元数据策略不受支持': 'Metadata policy is not supported', '去元数据': 'Strip metadata', '去字幕': 'Strip subtitles',
    '目标体积必须大于 0': 'Target size must be greater than 0', '目标码率必须大于 0': 'Target bitrate must be greater than 0',
    '自定义分辨率必须在 144 到 16384 像素之间': 'Custom resolution must be between 144 and 16384 pixels',
    '下载中': 'Downloading',
    '当前由「': 'These settings follow ',
    '」接管这些参数；改动任意一项会切到自定义，并保留你要改的那一项。': '. Changing any one of them switches to Custom and keeps your change.'
  });
  const replacements = Object.entries(ENGLISH).sort((a, b) => b[0].length - a[0].length);

  function normalizeLocale(value) {
    const locale = String(value == null ? '' : value).trim().replace(/_/g, '-').split(/[.@]/, 1)[0].toLowerCase();
    if (!locale || locale === 'c' || locale === 'posix') return '';
    return /^[a-z]{2,3}(?:-[a-z0-9]{1,8})*$/.test(locale) ? locale : '';
  }

  function normalizePreference(value) {
    return value === 'zh' || value === 'en' ? value : 'auto';
  }

  function resolveUiLanguage(locales, preference) {
    const selected = normalizePreference(preference);
    if (selected !== 'auto') return selected;
    const candidates = Array.isArray(locales) ? locales : [locales];
    for (const candidate of candidates) {
      const locale = normalizeLocale(candidate);
      if (locale) return locale === 'zh' || locale.startsWith('zh-') ? 'zh' : 'en';
    }
    return 'en';
  }

  function readPreference(storage) {
    try { return normalizePreference(storage && storage.getItem(PREFERENCE_KEY)); } catch (_) { return 'auto'; }
  }

  function getStorage() {
    try { return root && root.localStorage; } catch (_) { return null; }
  }

  function getLocales(navigator) {
    return [...(Array.isArray(navigator && navigator.languages) ? navigator.languages : []), navigator && navigator.language];
  }

  function translateText(value, language) {
    let text = String(value == null ? '' : value);
    if ((language || currentLanguage) !== 'en') return text;
    for (const [source, translated] of replacements) text = text.split(source).join(translated);
    return text;
  }

  /// 引擎/后端的原话不是界面文案，不许走上面那条"最长优先子串替换"。
  /// 子串替换会把「解码失败：文件不是有效图片」切成半译的「解码Failed：文件不是有效Images」——
  /// 一句话被换掉两个词，比留着原文更难读。这里只认整句词条：词典里有就换，没有就原样给出。
  function translateMessage(value, language) {
    const text = String(value == null ? '' : value);
    if ((language || currentLanguage) !== 'en') return text;
    const hit = ENGLISH[text];
    return hit === undefined ? text : hit;
  }

  let currentLanguage = 'en';
  let currentPreference = 'auto';
  const originalElementText = new WeakMap();
  const boundSelectors = new WeakSet();

  function applyToDocument(document, locales, preference) {
    if (!document) return currentLanguage;
    currentPreference = normalizePreference(preference == null ? readPreference(getStorage()) : preference);
    currentLanguage = resolveUiLanguage(locales == null ? getLocales(root && root.navigator) : locales, currentPreference);
    document.documentElement.lang = currentLanguage === 'zh' ? 'zh-Hans' : 'en';
    document.documentElement.dataset.uiLanguage = currentLanguage;
    document.documentElement.dataset.languagePreference = currentPreference;
    document.title = translateText(document.title, currentLanguage);

    const apply = () => {
      const scope = document.body || document.documentElement;
      for (const element of scope.querySelectorAll('[data-i18n-zh]')) {
        if (!originalElementText.has(element)) originalElementText.set(element, element.textContent);
        element.textContent = currentLanguage === 'zh' ? element.getAttribute('data-i18n-zh') : originalElementText.get(element);
      }

      const NodeFilterType = document.defaultView && document.defaultView.NodeFilter;
      const walker = document.createTreeWalker(scope, NodeFilterType ? NodeFilterType.SHOW_TEXT : 4);
      let node;
      while ((node = walker.nextNode())) {
        node.nodeValue = translateText(node.nodeValue, currentLanguage);
      }

      const translatedAttributes = ['title', 'aria-label', 'aria-description', 'placeholder', 'alt'];
      for (const element of scope.querySelectorAll('*')) {
        for (const attribute of translatedAttributes) {
          if (element.hasAttribute(attribute)) {
            const value = element.getAttribute(attribute);
            element.setAttribute(attribute, translateText(value, currentLanguage));
          }
        }
      }

      const selector = document.getElementById('languagePreference');
      if (selector) {
        selector.value = currentPreference;
        if (!boundSelectors.has(selector)) {
          boundSelectors.add(selector);
          selector.addEventListener('change', () => {
            const nextPreference = normalizePreference(selector.value);
            try { root.localStorage.setItem(PREFERENCE_KEY, nextPreference); } catch (_) {}
            if (root && root.location && typeof root.location.reload === 'function') root.location.reload();
          });
        }
      }
    };

    if (document.body) apply();
    else document.addEventListener('DOMContentLoaded', apply, { once: true });
    return currentLanguage;
  }

  const api = Object.freeze({
    preferenceKey: PREFERENCE_KEY,
    normalizeLocale,
    normalizePreference,
    resolveUiLanguage,
    translateText,
    translateMessage,
    readPreference,
    applyUiLanguage: applyToDocument,
    applyToDocument,
    get language() { return currentLanguage; },
    get preference() { return currentPreference; }
  });
  if (typeof module !== 'undefined' && module.exports) module.exports = api;

  if (root && root.document) {
    root.SizqoI18n = api;
    api.applyToDocument(root.document);
  }
})(typeof window === 'undefined' ? globalThis : window);
