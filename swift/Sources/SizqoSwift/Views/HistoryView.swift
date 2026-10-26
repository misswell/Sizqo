import SwiftUI
import AppKit

// MARK: - 历史页文案（与 frontend/app.js 的 historyRow 逐项对齐）

func historyParentDir(_ path: String) -> String {
    (path as NSString).deletingLastPathComponent
}

func historyTimeText(_ millis: Int64) -> String {
    guard millis > 0 else { return "" }
    let date = Date(timeIntervalSince1970: Double(millis) / 1000.0)
    let calendar = Calendar.current
    let clock = String(
        format: "%02d:%02d",
        calendar.component(.hour, from: date),
        calendar.component(.minute, from: date)
    )
    if calendar.isDateInToday(date) { return "今天 \(clock)" }
    if calendar.isDateInYesterday(date) { return "昨天 \(clock)" }
    return String(
        format: "%04d-%02d-%02d %@",
        calendar.component(.year, from: date),
        calendar.component(.month, from: date),
        calendar.component(.day, from: date),
        clock
    )
}

func historyStatusText(_ entry: HistoryEntry) -> String {
    if entry.status == .restored {
        guard let restoredAt = entry.restoredAt else { return "已恢复" }
        return "已恢复 · \(historyTimeText(restoredAt))"
    }
    if entry.status == .recoveryAvailable {
        return entry.backupExists ? "检测到可恢复的原图备份" : "原图备份已清理"
    }
    if entry.outputMode != "replace" { return "原图未覆盖" }
    if !entry.sourceExists { return "原文件位置不存在" }
    if !entry.backupExists { return "原图备份已清理" }
    return "已压缩"
}

/// 重建条目没有这次压缩的明细，报一个算出来的 0.0% 节省率是假数字。
func historySavingsText(_ entry: HistoryEntry) -> String {
    entry.status == .recoveryAvailable ? "明细已丢失" : "节省 \(String(format: "%.1f", abs(entry.savings)))%"
}

/// 算法位上写的是"这条记录怎么来的"：重建条目不是任何一次真实压缩。
func historyAlgorithmText(_ entry: HistoryEntry) -> String {
    entry.status == .recoveryAvailable ? "按备份重建" : (entry.algorithm.isEmpty ? entry.outType : entry.algorithm)
}

// MARK: - 历史行（.history-item：min-height 40，11px，与队列行同密度）

struct HistoryRowView: View {
    @EnvironmentObject var appState: AppState
    let entry: HistoryEntry

    private var isRecovery: Bool { entry.status == .recoveryAvailable }

    var body: some View {
        HStack(spacing: 8) {
            ZStack {
                Circle().fill(Color(nsColor: .separatorColor).opacity(0.35))
                SizqoIcon(name: rowIcon, size: 12, color: rowIconColor)
            }
            .frame(width: 24, height: 24)

            VStack(alignment: .leading, spacing: 1) {
                Text(entry.fileName)
                    .font(.system(size: 11, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(entry.sourcePath)
                Text("\(historyParentDir(entry.sourcePath)) · \(historyStatusText(entry))")
                    .font(.system(size: 10))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            VStack(alignment: .trailing, spacing: 1) {
                Text("\(CompressResult.formatBytesJS(entry.originalSize)) → \(CompressResult.formatBytesJS(entry.compressedSize))")
                    .font(.system(size: 10))
                    .monospacedDigit()
                Text(historySavingsText(entry))
                    .font(.system(size: 10))
                    .foregroundColor(isRecovery ? .secondary : (entry.savings >= 0 ? .green : .orange))
            }
            .frame(width: 132, alignment: .trailing)

            VStack(alignment: .trailing, spacing: 1) {
                Text(historyTimeText(entry.createdAt))
                    .font(.system(size: 10))
                    .foregroundColor(.secondary)
                    .monospacedDigit()
                Text(historyAlgorithmText(entry))
                    .font(.system(size: 10))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }
            .frame(width: 104, alignment: .trailing)

            HStack(spacing: 2) {
                // 与队列「压缩完成」那一行同一套按钮，按这条记录真实能做到的事给。
                // 判据在服务层（historyRowActions），视图只负责摆 —— 前端 app.js 是同一份顺序。
                ForEach(historyRowActions(entry), id: \.self) { action in
                    let blocked = appState.historyActionBlocked(action)
                    Button { perform(action) } label: {
                        SizqoIcon(name: action.icon, size: 14)
                    }
                    .buttonStyle(RowActionButtonStyle())
                    .disabled(blocked)
                    .opacity(blocked ? 0.45 : 1)
                    .help(blocked ? appState.restoreBlockedText : action.title)
                }
            }
            .fixedSize()
        }
        .padding(.horizontal, AppMetrics.sectionHPadding)
        .padding(.vertical, 6)
        .frame(minHeight: 40)
        .opacity(entry.status == .restored || isRecovery ? 0.55 : 1)
    }

    private func perform(_ action: HistoryRowAction) {
        switch action {
        case .saveAs: appState.saveHistoryOutput(entry)
        case .compare: appState.compareHistoryEntry(entry)
        case .restore: appState.restoreHistoryEntry(id: entry.id)
        case .deleteOutput: appState.deleteHistoryOutput(entry)
        case .finder: appState.openInFinder(path: finderTarget(entry))
        case .copyLog: appState.copyHistoryLog(entry)
        }
    }

    /// 重建条目用警告图标而不是对勾：它不是一次成功的压缩，是一次数据抢救。
    private var rowIcon: String {
        if isRecovery { return SizqoIconName.warning }
        return entry.status == .restored ? SizqoIconName.restore : SizqoIconName.check
    }

    private var rowIconColor: Color {
        if isRecovery { return .orange }
        return entry.status == .restored ? .secondary : .green
    }

    /// 压缩结果与源文件不是同一个时优先跳到结果，否则回到源路径。
    private func finderTarget(_ entry: HistoryEntry) -> String {
        if let output = entry.outputPath,
           output != entry.sourcePath,
           entry.status == .compressed,
           FileManager.default.fileExists(atPath: output) {
            return output
        }
        return entry.sourcePath
    }
}

// MARK: - 历史工作区（rail「历史」目的地：历史列表列 + 详情列，与本轮同构）
//
// 契约：列表行只负责选中；完整可执行动作集（另存为 / 对比 / 恢复 / 删除结果 / 访达 / 日志）
// 在详情栏带文字展示，判据与行内图标同源（historyRowActions，视图不另算一套）。

struct HistoryWorkspaceView: View {
    @EnvironmentObject var appState: AppState

    private var selectedEntry: HistoryEntry? {
        guard let id = appState.selectedHistoryID else { return nil }
        return appState.historyEntries.first { $0.id == id }
    }

    var body: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                header
                HairlineDivider()
                listColumn
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(SizqoPalette.surface)

            HairlineDivider()
            HistoryDetailColumnView(entry: selectedEntry)
                .frame(width: AppMetrics.detailColumnWidth)
                .background(SizqoPalette.window)
        }
        // 每次进入都重新读一遍：压缩、恢复可能刚刚改过历史。
        .onAppear { appState.refreshHistory() }
    }

    private var header: some View {
        HStack(spacing: 6) {
            SizqoIcon(name: SizqoIconName.history, size: 14, color: .accentColor)
            Text("\(appState.historyEntries.count) 条记录")
                .font(SizqoType.control)
            Spacer()
            Button {
                appState.clearHistory()
            } label: {
                Label {
                    Text("清空历史")
                } icon: {
                    SizqoIcon(name: SizqoIconName.trash, size: 14)
                }
                .labelIconToTextSpacing(3)
            }
            .buttonStyle(GhostButtonStyle())
            // 清空 = 连原图备份一起删。真正的判据在后端（clearHistory 还会看事务）。
            .disabled(appState.historyEntries.isEmpty || appState.scheduler.isBatchActive)
            .help("清空历史记录，以及 Sizqo 保存的原图备份")
        }
        .padding(.horizontal, SizqoSpacing.rowX)
        .padding(.vertical, 7)
    }

    @ViewBuilder private var listColumn: some View {
        if appState.historyEntries.isEmpty {
            VStack(spacing: 6) {
                Spacer()
                SizqoIcon(name: SizqoIconName.history, size: 26, color: .secondary.opacity(0.5))
                Text("还没有压缩记录")
                    .font(SizqoType.body)
                    .foregroundColor(.secondary)
                Text("完成一次压缩后，这里会留下可以反悔的入口")
                    .font(SizqoType.caption)
                    .foregroundColor(SizqoPalette.tertiaryText)
                Spacer()
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(appState.historyEntries) { entry in
                        HistorySelectableRowView(entry: entry)
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
            }
            .frame(maxHeight: .infinity)
        }
    }
}

/// 可选中历史行：内容复用 HistoryRowView，选中态与队列行同一判据（强调描边 + 软底）。
private struct HistorySelectableRowView: View {
    @EnvironmentObject var appState: AppState
    let entry: HistoryEntry

    var body: some View {
        HistoryRowView(entry: entry)
            .background(
                RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                    .fill(selected ? SizqoPalette.emphasisSoft : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                    .strokeBorder(selected ? Color.accentColor : Color.clear, lineWidth: 1)
            )
            .contentShape(Rectangle())
            .onTapGesture { appState.selectedHistoryID = entry.id }
    }

    private var selected: Bool { appState.selectedHistoryID == entry.id }
}

// MARK: - 历史详情列（选中记录 = 明细 + 此刻真能做的动作；空态 = 历史总览）

private struct HistoryDetailColumnView: View {
    @EnvironmentObject var appState: AppState
    let entry: HistoryEntry?

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 5) {
                SizqoIcon(name: SizqoIconName.eye, size: 14, color: .accentColor)
                Text(entry == nil ? "历史总览" : "详情")
                    .font(SizqoType.control)
                Spacer()
                if let entry {
                    Text(entry.fileName)
                        .font(SizqoType.micro)
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .padding(.horizontal, SizqoSpacing.rowX)
            .padding(.vertical, 10)

            HairlineDivider()

            ScrollView {
                VStack(alignment: .leading, spacing: SizqoSpacing.md) {
                    if let entry {
                        HistoryEntryDetailView(entry: entry)
                    } else {
                        HistoryTotalsView()
                    }
                }
                .padding(SizqoSpacing.md)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

private struct HistoryEntryDetailView: View {
    @EnvironmentObject var appState: AppState
    let entry: HistoryEntry

    private var isRecovery: Bool { entry.status == .recoveryAvailable }

    var body: some View {
        VStack(alignment: .leading, spacing: SizqoSpacing.md) {
            preview
            rows
            actionsBlock
        }
    }

    /// 预览指向"这次压缩的结果"；结果已不在就退回原图位置。
    @ViewBuilder private var preview: some View {
        let path = (entry.outputExists ? entry.outputPath : nil) ?? entry.sourcePath
        if let img = NSImage(contentsOfFile: path) {
            Image(nsImage: img)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(maxHeight: 220)
                .clipShape(RoundedRectangle(cornerRadius: SizqoRadius.card, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: SizqoRadius.card, style: .continuous)
                        .strokeBorder(SizqoPalette.hairline, lineWidth: 0.5)
                )
        } else {
            RoundedRectangle(cornerRadius: SizqoRadius.card, style: .continuous)
                .fill(SizqoPalette.surfaceSunken)
                .frame(height: 90)
                .overlay(
                    Text("文件已不在，无法预览")
                        .font(SizqoType.caption)
                        .foregroundColor(.secondary)
                )
        }
    }

    private var rows: some View {
        VStack(alignment: .leading, spacing: 5) {
            detailRow("状态", historyStatusText(entry),
                      entry.status == .restored || isRecovery ? .secondary : .green)
            detailRow("体积", "\(CompressResult.formatBytesJS(entry.originalSize)) → \(CompressResult.formatBytesJS(entry.compressedSize))")
            detailRow("节省", historySavingsText(entry),
                      isRecovery ? .secondary : (entry.savings >= 0 ? .green : .orange))
            detailRow("算法", historyAlgorithmText(entry))
            detailRow("输出格式", entry.outType.isEmpty ? "(未知)" : entry.outType)
            detailRow("时间", historyTimeText(entry.createdAt))
            detailRow("原路径", entry.sourcePath)
            if let output = entry.outputPath, output != entry.sourcePath {
                detailRow("结果", output)
            }
            if entry.outputMode == "replace" {
                detailRow("备份", entry.backupExists ? "原图备份在" : "原图备份已清理",
                          entry.backupExists ? .primary : SizqoPalette.tertiaryText)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// 动作集与行内图标同源（historyRowActions），这里只是带文字再摆一遍。
    private var actionsBlock: some View {
        let actions = historyRowActions(entry)
        return VStack(alignment: .leading, spacing: 6) {
            if !actions.isEmpty {
                HairlineDivider()
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 130), spacing: 6)], alignment: .leading, spacing: 6) {
                    ForEach(actions, id: \.self) { action in
                        let blocked = appState.historyActionBlocked(action)
                        Button {
                            perform(action)
                        } label: {
                            HStack(spacing: 4) {
                                SizqoIcon(name: action.icon, size: 13)
                                Text(action.title).font(SizqoType.captionMedium)
                                Spacer(minLength: 0)
                            }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 5)
                            .background(
                                RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                                    .fill(SizqoPalette.surface)
                            )
                            .overlay(
                                RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                                    .strokeBorder(SizqoPalette.hairline, lineWidth: 0.5)
                            )
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(blocked)
                        .opacity(blocked ? 0.45 : 1)
                        .help(blocked ? appState.restoreBlockedText : action.title)
                    }
                }
            }
        }
    }

    private func perform(_ action: HistoryRowAction) {
        switch action {
        case .saveAs: appState.saveHistoryOutput(entry)
        case .compare: appState.compareHistoryEntry(entry)
        case .restore: appState.restoreHistoryEntry(id: entry.id)
        case .deleteOutput: appState.deleteHistoryOutput(entry)
        case .finder: appState.openInFinder(path: finderTarget(entry))
        case .copyLog: appState.copyHistoryLog(entry)
        }
    }

    private func finderTarget(_ entry: HistoryEntry) -> String {
        if let output = entry.outputPath,
           output != entry.sourcePath,
           entry.status == .compressed,
           FileManager.default.fileExists(atPath: output) {
            return output
        }
        return entry.sourcePath
    }

    private func detailRow(_ label: String, _ value: String, _ color: Color = .primary) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(label)
                .font(SizqoType.caption)
                .foregroundColor(.secondary)
                .frame(width: 56, alignment: .leading)
            Text(value)
                .font(SizqoType.digit(11))
                .foregroundColor(color)
                .lineLimit(2)
                .truncationMode(.middle)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}

/// 未选中记录时的空态：只报历史里真有的数，不编造。
private struct HistoryTotalsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        let entries = appState.historyEntries
        let compressed = entries.filter { $0.status == .compressed }
        let recoveries = entries.filter { $0.status == .recoveryAvailable }.count
        let totalSaved = compressed.reduce(Int64(0)) { $0 + $1.originalSize - $1.compressedSize }

        VStack(alignment: .leading, spacing: 8) {
            Text("历史")
                .font(SizqoType.captionMedium)
                .foregroundColor(.secondary)
            HStack(spacing: SizqoSpacing.lg) {
                stat("记录", "\(entries.count)", .primary)
                stat("已压缩", "\(compressed.count)", .accentColor)
                stat("已恢复", "\(entries.filter { $0.status == .restored }.count)", .secondary)
                stat("累计体积变化", CompressResult.formatBytesJS(max(0, totalSaved)), .green)
            }
            Text("选中一条记录查看明细和此刻能做的动作。"
                 + (recoveries > 0 ? "另有 \(recoveries) 条按备份重建的记录。" : ""))
                .font(SizqoType.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func stat(_ label: String, _ value: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label)
                .font(SizqoType.micro)
                .foregroundColor(SizqoPalette.tertiaryText)
            Text(value)
                .font(SizqoType.statValue.monospacedDigit())
                .foregroundColor(color)
        }
    }
}
