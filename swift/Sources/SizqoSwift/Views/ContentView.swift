import SwiftUI
import UniformTypeIdentifiers
import AppKit

// 两栏工作区 + 意图优先（core/intent-model.md）：
//   rail（本轮 / 历史 / 设置） │ 工作列（意图栏 → 媒体列表 → 列表筛选） │ 详情栏
//   运行条钉在窗口底部，有工作就可见；切换 rail 目的地不触碰队列与运行中的会话。

struct ContentView: View {
    @EnvironmentObject var appState: AppState
    @State private var showSystemInfo = false

    var body: some View {
        ZStack(alignment: .bottom) {
            VStack(spacing: 0) {
                TitleBarView(showSystemInfo: $showSystemInfo)
                HairlineDivider()
                // §3.4 人工恢复现场：自动恢复证明不了归属时，把双方状态摆给用户拍板。
                if !appState.manualRecoveries.isEmpty {
                    ManualRecoveryBanner(items: appState.manualRecoveries) { item, restore, confirmVoucherMismatch in
                        if let error = appState.resolveManualRecovery(
                            item,
                            restore: restore,
                            confirmOverwrite: restore,
                            confirmVoucherMismatch: confirmVoucherMismatch
                        ) {
                            appState.showToast(error)
                        }
                    }
                    HairlineDivider()
                }

                HStack(spacing: 0) {
                    RailView()
                    HairlineDivider()
                    Group {
                        switch appState.page {
                        case .main:
                            WorkspaceView(showSystemInfo: $showSystemInfo)
                        case .history:
                            HistoryWorkspaceView()
                        case .settings:
                            SettingsWorkspaceView()
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }

                // 运行条：只要队列里还有工作（含停止后剩下的 pending）就常驻。
                if !appState.items.isEmpty || appState.isCompressing {
                    HairlineDivider()
                    RunBarView()
                }
            }
            .background(SizqoPalette.window)

            // Toast（bottom 20，黑 0.8，12px，胶囊）
            if appState.toastVisible, let msg = appState.toastMessage {
                Text(msg)
                    .font(SizqoType.body)
                    .foregroundColor(.white)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                    .background(
                        Capsule(style: .continuous)
                            .fill(Color.black.opacity(0.8))
                            .shadow(color: Color.black.opacity(0.2), radius: 6, y: 2)
                    )
                    .padding(.bottom, 20)
                    .transition(.opacity)
            }

            if showSystemInfo {
                SystemInfoModal(isPresented: $showSystemInfo)
            }
        }
        .animation(.easeOut(duration: 0.2), value: appState.toastVisible)
        .onAppear {
            appState.detectVersion()
            if let win = NSApp.windows.first(where: { $0.isVisible && $0.title == "Sizqo" })
                ?? NSApp.windows.first(where: { $0.isVisible }) {
                win.identifier = NSUserInterfaceItemIdentifier("main")
                win.isMovableByWindowBackground = true
            }
            NotificationCenter.default.addObserver(
                forName: NSNotification.Name("MainRestoreFile"),
                object: nil,
                queue: .main
            ) { notification in
                if let path = notification.userInfo?["filePath"] as? String {
                    Task { @MainActor in
                        appState.markRestored(path: path)
                    }
                }
            }
        }
    }
}

// MARK: - Title Bar（rail 接管导航后只剩拖动区 + 版本）

struct TitleBarView: View {
    @EnvironmentObject var appState: AppState
    @Binding var showSystemInfo: Bool

    var body: some View {
        ZStack {
            HStack(spacing: 5) {
                Image(nsImage: NSImage(named: "AppIcon") ?? NSImage())
                    .resizable()
                    .frame(width: 16, height: 16)
                    .clipShape(RoundedRectangle(cornerRadius: 2, style: .continuous))
                Text("Sizqo (Swift)")
                    .font(SizqoType.captionMedium)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }

            HStack(spacing: 2) {
                Color.clear.frame(width: 70, height: 1)
                Spacer()
                Button {
                    showSystemInfo = true
                } label: {
                    SizqoIcon(name: SizqoIconName.info, size: 15)
                }
                .buttonStyle(TitleBarButtonStyle())
                .help("系统转换说明")

                Text("v\(appState.appVersion)")
                    .font(SizqoType.caption)
                    .foregroundColor(SizqoPalette.tertiaryText)
                    .padding(.trailing, 10)
            }
        }
        .frame(height: AppMetrics.titleBarHeight)
        .background(SizqoPalette.window)
    }
}

// MARK: - Rail（本轮 / 历史 / 设置；纯导航，切换绝不影响队列或运行中的会话）

struct RailView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(spacing: 4) {
            ForEach(AppPage.allCases, id: \.self) { page in
                RailItemView(page: page)
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 10)
        .frame(width: AppMetrics.railWidth)
        .background(SizqoPalette.window)
    }
}

private struct RailItemView: View {
    @EnvironmentObject var appState: AppState
    let page: AppPage

    private var icon: String {
        switch page {
        case .main: return SizqoIconName.queue
        case .history: return SizqoIconName.history
        case .settings: return SizqoIconName.settings
        }
    }

    private var selected: Bool { appState.page == page }

    var body: some View {
        Button {
            appState.showPage(page)
        } label: {
            VStack(spacing: 3) {
                ZStack(alignment: .topTrailing) {
                    SizqoIcon(
                        name: icon, size: 19,
                        color: selected ? .accentColor : .secondary
                    )
                    // 运行指示：会话非 idle 就点亮，与后端阶段同源，不看按钮文案。
                    if page == .main && appState.compressionPhase != .idle {
                        Circle()
                            .fill(SizqoPalette.running)
                            .frame(width: 6, height: 6)
                            .offset(x: 3, y: -2)
                    }
                }
                Text(page.railLabel)
                    .font(SizqoType.micro)
                    .foregroundColor(selected ? .accentColor : .secondary)
            }
            .frame(width: 56, height: 44)
            .background(
                RoundedRectangle(cornerRadius: SizqoRadius.chip, style: .continuous)
                    .fill(selected ? SizqoPalette.accentSoft : Color.clear)
            )
            .contentShape(RoundedRectangle(cornerRadius: SizqoRadius.chip, style: .continuous))
        }
        .buttonStyle(.plain)
        .help(page.railLabel)
    }
}

// MARK: - 工作列（意图栏 → 媒体列表 → 列表筛选）+ 详情栏

struct WorkspaceView: View {
    @EnvironmentObject var appState: AppState
    @Binding var showSystemInfo: Bool
    @State private var isDragOver = false

    var body: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                IntentBarView(showSystemInfo: $showSystemInfo)
                HairlineDivider()
                mediaColumn
                HairlineDivider()
                listFilters
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(SizqoPalette.surface)

            HairlineDivider()
            DetailColumnView()
                .frame(width: AppMetrics.detailColumnWidth)
                .background(SizqoPalette.window)
        }
        .background(
            RoundedRectangle(cornerRadius: 0).fill(Color.clear)
        )
        .onDrop(of: [.fileURL], isTargeted: $isDragOver) { handleDrop(providers: $0) }
        .overlay(dragOverlay)
    }

    /// 空队列 = 拖拽区；有文件 = 队列列表。追加文件入口保留在列表头部。
    @ViewBuilder private var mediaColumn: some View {
        if appState.items.isEmpty {
            DropZoneView(isDragOver: $isDragOver)
                .padding(AppMetrics.containerPadding)
        } else {
            VStack(spacing: 0) {
                listHeader
                Divider().opacity(0.4)
                fileList
            }
        }
    }

    private var listHeader: some View {
        HStack(spacing: 6) {
            SizqoIcon(name: SizqoIconName.queue, size: 14, color: .accentColor)
            Text("\(appState.items.count) 个文件")
                .font(SizqoType.control)
            Spacer()
            Button {
                appState.addFilesPanel()
            } label: {
                Label { Text("追加") } icon: { SizqoIcon(name: SizqoIconName.addImages, size: 14) }
                    .labelIconToTextSpacing(3)
            }
            .buttonStyle(GhostButtonStyle())
            .help("追加文件")
            Button {
                appState.addFolder()
            } label: {
                Label { Text("文件夹") } icon: { SizqoIcon(name: SizqoIconName.folderAdd, size: 14) }
                    .labelIconToTextSpacing(3)
            }
            .buttonStyle(GhostButtonStyle())
            .help("追加文件夹")
            Button {
                appState.clearQueue()
            } label: {
                SizqoIcon(name: SizqoIconName.trash, size: 14)
            }
            .buttonStyle(RowActionButtonStyle())
            .help("清空列表")
        }
        .padding(.horizontal, SizqoSpacing.rowX)
        .padding(.vertical, 7)
    }

    private var fileList: some View {
        ScrollView {
            LazyVStack(spacing: 2) {
                if appState.displayItems.isEmpty && appState.onlyShowFailed {
                    Text("没有失败的文件")
                        .font(SizqoType.body)
                        .foregroundColor(.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 28)
                } else {
                    ForEach(appState.displayItems) { item in
                        FileRowView(item: item)
                    }
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
        }
        .frame(maxHeight: .infinity)
    }

    // 列表筛选 / 排序在列表之后（契约：intent bar → media list → list filters）。
    // 本构建线只有图片，没有视频，故不提供媒体种类筛选。
    private var listFilters: some View {
        HStack(spacing: 8) {
            Toggle("只显示失败", isOn: $appState.onlyShowFailed)
                .toggleStyle(.checkbox)
                .font(SizqoType.caption)
            Spacer()
            Picker("排序", selection: $appState.sortKey) {
                ForEach(SortKey.allCases, id: \.self) { key in
                    Text(key.label).tag(key)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.small)
            .frame(width: 116)
            .font(SizqoType.caption)
            Button {
                appState.sortAscending.toggle()
            } label: {
                HStack(spacing: 3) {
                    SizqoIcon(
                        name: appState.sortAscending ? SizqoIconName.sortAsc : SizqoIconName.sortDesc,
                        size: 13
                    )
                    Text(appState.sortAscending ? "升序" : "降序")
                        .font(SizqoType.caption)
                }
            }
            .buttonStyle(GhostButtonStyle())
        }
        .foregroundColor(.secondary)
        .padding(.horizontal, SizqoSpacing.rowX)
        .padding(.vertical, 6)
        .background(SizqoPalette.surfaceSunken)
    }

    @ViewBuilder private var dragOverlay: some View {
        if isDragOver {
            RoundedRectangle(cornerRadius: 0)
                .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
                .background(Color.accentColor.opacity(0.05))
                .allowsHitTesting(false)
        }
    }

    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        var urls: [URL] = []
        let group = DispatchGroup()
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
            group.enter()
            provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, _ in
                defer { group.leave() }
                if let data = item as? Data,
                   let url = URL(dataRepresentation: data, relativeTo: nil) {
                    DispatchQueue.main.async { urls.append(url) }
                } else if let url = item as? URL {
                    DispatchQueue.main.async { urls.append(url) }
                }
            }
        }
        group.notify(queue: .main) { appState.addFiles(paths: urls.map(\.path)) }
        return true
    }
}

// MARK: - Drop Zone（空队列时的工作列内容）

struct DropZoneView: View {
    @EnvironmentObject var appState: AppState
    @Binding var isDragOver: Bool

    var body: some View {
        VStack(spacing: 0) {
            ZStack {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Color.accentColor.opacity(0.10))
                    .frame(width: 48, height: 48)
                SizqoIcon(name: SizqoIconName.addImages, size: 32, color: .accentColor)
            }
            .padding(.bottom, 8)
            .offset(y: isDragOver ? -1 : 0)

            Text(isDragOver ? "松开以添加" : "拖拽图片或文件夹到此处")
                .font(.system(size: 14, weight: .medium))
                .padding(.bottom, 3)

            Text("支持 PNG、JPG、GIF、WebP、BMP 格式 · 本构建仅处理图片")
                .font(SizqoType.caption)
                .foregroundColor(.secondary)
                .padding(.bottom, 10)

            HStack(spacing: 8) {
                Button {
                    appState.addFilesPanel()
                } label: {
                    Label { Text("选择文件") } icon: { SizqoIcon(name: SizqoIconName.addImages, size: 16) }
                        .labelIconToTextSpacing(4)
                }
                .buttonStyle(PrimaryButtonStyle())

                Button {
                    appState.addFolder()
                } label: {
                    Label { Text("选择文件夹") } icon: { SizqoIcon(name: SizqoIconName.folderAdd, size: 16) }
                        .labelIconToTextSpacing(4)
                }
                .buttonStyle(SecondaryButtonStyle())
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(18)
        .contentShape(Rectangle())
        .onTapGesture { appState.addFilesPanel() }
        .background(
            RoundedRectangle(cornerRadius: AppMetrics.cardRadius, style: .continuous)
                .fill(isDragOver ? Color.accentColor.opacity(0.06) : SizqoPalette.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: AppMetrics.cardRadius, style: .continuous)
                .strokeBorder(
                    isDragOver ? Color.accentColor : Color(nsColor: .separatorColor),
                    style: StrokeStyle(lineWidth: 1, dash: [5, 4])
                )
        )
        .animation(.easeOut(duration: 0.15), value: isDragOver)
    }
}

// MARK: - 意图栏（image intent 表逐字实现 + 正交的 输出方式 / 自动开始 + 原始参数折叠）

struct IntentBarView: View {
    @EnvironmentObject var appState: AppState
    @Binding var showSystemInfo: Bool
    @State private var maxDimDraft = ""
    @State private var budgetDraftKiB = ""

    var body: some View {
        VStack(spacing: 0) {
            headerRow

            if appState.options.processingMode == .advanced {
                chips
                intentHelp
            } else {
                systemParams
            }

            HairlineDivider()
            orthogonalRow
            HairlineDivider()
            disclosure
        }
        .padding(.horizontal, SizqoSpacing.rowX)
        .padding(.vertical, SizqoSpacing.sm)
        .background(SizqoPalette.surface)
        .onAppear { syncDrafts() }
        .onChange(of: appState.intent) { _ in syncDrafts() }
        .onChange(of: appState.options.quality) { _ in syncDrafts() }
    }

    private var headerRow: some View {
        HStack(spacing: 6) {
            SizqoIcon(name: SizqoIconName.compress, size: 14, color: .accentColor)
            Text("本轮意图")
                .font(SizqoType.control)
            Spacer()
            // 处理方式也是真控件：引擎两条路径都读它（系统转换 / 高级压缩）。
            Picker("", selection: $appState.options.processingMode) {
                Text("系统转换").tag(ProcessingMode.system)
                Text("高级压缩").tag(ProcessingMode.advanced)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .controlSize(.mini)
            .fixedSize()
            .onChange(of: appState.options.processingMode) { _ in appState.saveSettings() }
        }
        .padding(.bottom, SizqoSpacing.sm)
    }

    // 意图 chips：契约 Image 表六个，一个不多一个不少；无视频意图。
    private var chips: some View {
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 88), spacing: 6)],
            alignment: .leading, spacing: 6
        ) {
            ForEach(ImageIntent.allCases, id: \.self) { intent in
                IntentChipView(
                    title: intent == .balanced ? "均衡（默认）" : intent.label,
                    selected: appState.intent == intent,
                    icon: chipIcon(intent)
                ) {
                    appState.selectIntent(intent)
                    syncDrafts()
                }
            }
        }
        .padding(.bottom, 6)
    }

    private func chipIcon(_ intent: ImageIntent) -> String {
        switch intent {
        case .losslessFirst: return SizqoIconName.eye
        case .balanced: return SizqoIconName.sliders
        case .asSmallAsPossible: return SizqoIconName.minimize
        case .webSize: return SizqoIconName.globe
        case .targetSize: return SizqoIconName.targetSize
        case .custom: return SizqoIconName.settings
        }
    }

    private var intentHelp: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(appState.intent.helpText)
                .font(SizqoType.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            // 结果导向的诚实话：重编像素不带元数据，系统转换才保留。
            // 系统转换模式下这句不成立，所以只在高级路径说（与 Direct 的 note 一致）。
            if appState.options.processingMode == .advanced {
                Text(appState.intent.metadataNote)
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if appState.intent != .custom || appState.options.quality > 0 {
                Text("将发送：\(appState.activeIntentSummary)")
                    .font(SizqoType.caption)
                    .foregroundColor(SizqoPalette.tertiaryText)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, SizqoSpacing.sm)
    }

    // 正交项：输出方式与自动开始必须常显可编辑（契约）。
    private var orthogonalRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("输出方式")
                    .font(SizqoType.bodyMedium)
                    .foregroundColor(.secondary)
                Picker("", selection: $appState.options.outputMode) {
                    Text("覆盖原文件").tag(OutputMode.replace)
                    Text("加后缀").tag(OutputMode.suffix)
                    Text("输出到目录").tag(OutputMode.folder)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)
                .fixedSize()
                .onChange(of: appState.options.outputMode) { _ in appState.saveSettings() }

                if appState.options.outputMode == .suffix {
                    TextField("_compressed", text: $appState.options.outputSuffix)
                        .textFieldStyle(.roundedBorder)
                        .controlSize(.small)
                        .frame(width: 110)
                        .onChange(of: appState.options.outputSuffix) { _ in appState.saveSettings() }
                }
                if appState.options.outputMode == .folder {
                    Button {
                        _ = appState.pickOutputDir()
                    } label: {
                        HStack(spacing: 3) {
                            SizqoIcon(name: SizqoIconName.folderOpen, size: 13)
                            Text(appState.options.outputDir.map { ($0 as NSString).lastPathComponent } ?? "选择目录")
                                .font(SizqoType.caption)
                                .lineLimit(1)
                        }
                    }
                    .buttonStyle(SmallButtonStyle())
                }

                Spacer(minLength: 0)

                Text("自动开始")
                    .font(SizqoType.bodyMedium)
                    .foregroundColor(.secondary)
                Toggle("", isOn: $appState.autoCompress)
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .labelsHidden()
                    .onChange(of: appState.autoCompress) { _ in appState.saveSettings() }
            }
            if appState.options.outputMode == .folder && appState.options.outputDir == nil {
                Text("尚未选择输出目录，开始压缩前需要先选好")
                    .font(SizqoType.caption)
                    .foregroundColor(SizqoPalette.caution)
            }
        }
        .padding(.vertical, 7)
    }

    // 原始参数折叠区：默认折叠（初值判据由 HistoryStoreCheck [30] 钉住）。
    private var disclosure: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { appState.settingsExpanded.toggle() }
            } label: {
                HStack(spacing: 5) {
                    SizqoIcon(name: SizqoIconName.sliders, size: 13, color: .secondary)
                    Text("原始参数")
                        .font(SizqoType.captionMedium)
                        .foregroundColor(.secondary)
                    if appState.intent == .custom {
                        Text("（自定义中）")
                            .font(SizqoType.caption)
                            .foregroundColor(.accentColor)
                    }
                    Spacer()
                    SizqoIcon(
                        name: SizqoIconName.chevronDown, size: 12,
                        color: SizqoPalette.tertiaryText
                    )
                    .rotationEffect(.degrees(appState.settingsExpanded ? 180 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .padding(.top, 6)

            if appState.settingsExpanded && appState.options.processingMode == .advanced {
                rawAdvancedParams
                    .padding(.top, 2)
            }
        }
    }

    // 原始参数（advanced）：编辑任何一项 → 意图切 custom，屏幕立刻显示编译后的请求。
    private var rawAdvancedParams: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("质量")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                Slider(value: Binding(
                    get: { Double(appState.options.quality) },
                    set: { appState.setQuality(Int($0)) }
                ), in: 10...100, step: 1)
                .controlSize(.small)
                Text("\(appState.options.quality)")
                    .font(SizqoType.digit(11))
                    .foregroundColor(.secondary)
                    .frame(width: 26, alignment: .trailing)
            }
            HStack(spacing: 8) {
                Text("输出格式")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                Picker("", selection: Binding(
                    get: { appState.options.outputFormat },
                    set: { appState.setOutputFormat($0) }
                )) {
                    Text("保持原格式").tag(OutputFormat.original)
                    // §22：选项来自能力表，少了哪个编码器就不出现哪一项。
                    ForEach(OutputFormat.advancedTargets, id: \.self) { format in
                        Text(format.label).tag(format)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)
                .frame(width: 120, alignment: .leading)
                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                Text("尺寸上限")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                TextField("不限", text: $maxDimDraft)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                    .frame(width: 90)
                    .onChange(of: maxDimDraft) { v in commitMaxDim(v) }
                Text("px · 最长边，只缩不放大")
                    .font(SizqoType.micro)
                    .foregroundColor(SizqoPalette.tertiaryText)
                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                Text("体积预算")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                TextField("不限", text: $budgetDraftKiB)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                    .frame(width: 90)
                    .onChange(of: budgetDraftKiB) { v in commitBudget(v) }
                Text("KiB · 最多 6 次编码求解")
                    .font(SizqoType.micro)
                    .foregroundColor(SizqoPalette.tertiaryText)
                Spacer(minLength: 0)
            }
            Text("将发送：\(appState.activeIntentSummary)")
                .font(SizqoType.caption)
                .foregroundColor(.accentColor.opacity(0.85))
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.top, 6)
    }

    // 系统转换模式的原始参数（它就是这条线的全部控件，无假选项）。
    private var systemParams: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("格式")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                Picker("", selection: $appState.options.outputFormat) {
                    Text("原格式").tag(OutputFormat.original)
                    ForEach(OutputFormat.systemTargets, id: \.self) { format in
                        Text(format.label).tag(format)
                    }
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                .controlSize(.mini)
                .fixedSize()
                .onChange(of: appState.options.outputFormat) { _ in appState.saveSettings() }
                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                Text("图像大小")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                Picker("", selection: $appState.options.systemImageSize) {
                    ForEach(SystemImageSize.allCases, id: \.self) { s in
                        Text(s.label).tag(s)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)
                .frame(width: 180, alignment: .leading)
                .onChange(of: appState.options.systemImageSize) { _ in appState.saveSettings() }
                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                Text("元数据")
                    .font(SizqoType.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 56, alignment: .leading)
                Toggle("保留元数据", isOn: $appState.options.preserveMetadata)
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .font(SizqoType.caption)
                    .onChange(of: appState.options.preserveMetadata) { _ in appState.saveSettings() }
                Button {
                    showSystemInfo = true
                } label: {
                    SizqoIcon(name: SizqoIconName.info, size: 13)
                }
                .buttonStyle(RowActionButtonStyle())
                .help("系统转换说明")
                Spacer(minLength: 0)
            }
        }
        .padding(.vertical, 6)
    }

    private func syncDrafts() {
        maxDimDraft = appState.options.maxDimensionPx.map(String.init) ?? ""
        if let b = appState.options.maxOutputBytes {
            budgetDraftKiB = String(max(1, Int((Double(b) / 1024.0).rounded())))
        } else {
            budgetDraftKiB = ""
        }
    }

    private func commitMaxDim(_ raw: String) {
        let v = raw.trimmingCharacters(in: .whitespaces)
        if v.isEmpty {
            if appState.options.maxDimensionPx != nil { appState.setMaxDimension(nil) }
            return
        }
        guard let parsed = Int(v), parsed > 0 else { return } // 非法输入不落盘
        if parsed != appState.options.maxDimensionPx { appState.setMaxDimension(parsed) }
    }

    private func commitBudget(_ raw: String) {
        let v = raw.trimmingCharacters(in: .whitespaces)
        if v.isEmpty {
            if appState.options.maxOutputBytes != nil { appState.setBudget(nil) }
            return
        }
        guard let kib = Int(v), kib > 0 else { return }
        let bytes = Int64(kib) * 1024
        if bytes != appState.options.maxOutputBytes { appState.setBudget(bytes) }
    }
}

/// 意图 chip：选中态 = 强调底 + 描边，纯展示组件，点击行为由父级注入。
struct IntentChipView: View {
    let title: String
    let selected: Bool
    let icon: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                SizqoIcon(name: icon, size: 13, color: selected ? .accentColor : .secondary)
                Text(title)
                    .font(SizqoType.captionMedium)
                    .lineLimit(1)
                    .minimumScaleFactor(0.85)
            }
            .foregroundColor(selected ? .accentColor : .primary)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: SizqoRadius.chip, style: .continuous)
                    .fill(selected ? SizqoPalette.accentSoft : (hovering ? SizqoPalette.emphasisSoft : Color.clear))
            )
            .overlay(
                RoundedRectangle(cornerRadius: SizqoRadius.chip, style: .continuous)
                    .strokeBorder(selected ? Color.accentColor : SizqoPalette.hairline,
                                  lineWidth: selected ? 1 : 0.5)
            )
            .contentShape(RoundedRectangle(cornerRadius: SizqoRadius.chip, style: .continuous))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}

// MARK: - 队列行（.file-queue-item：min-height 40；点击选中进详情栏）

struct FileRowView: View {
    @EnvironmentObject var appState: AppState
    let item: QueueItem

    var body: some View {
        HStack(spacing: 8) {
            iconDisc

            Text(item.fileName)
                .font(SizqoType.caption)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)

            Text(sizeText)
                .font(SizqoType.digit(10))
                .foregroundColor(item.status == .done ? .secondary : SizqoPalette.tertiaryText)
                .fixedSize()

            HStack(spacing: 2) {
                Text(statusText)
                    .font(SizqoType.microBold)
                    .foregroundColor(statusColor)
                    .lineLimit(1)
                if !(item.result?.warnings ?? []).isEmpty || item.result?.targetMet == false {
                    SizqoIcon(name: SizqoIconName.warning, size: 10, color: .yellow)
                        .help((item.result?.warnings ?? []).joined(separator: "\n"))
                }
                // §70 后端原话只进这里和日志：句子由契约按 reasonCode 选，原话跟在后面备查。
                if let why = item.result?.contractDetailText, !why.isEmpty {
                    SizqoIcon(name: SizqoIconName.warning, size: 10, color: .yellow)
                        .help(why)
                }
            }
            .frame(minWidth: 44, alignment: .trailing)
            .fixedSize(horizontal: true, vertical: false)

            // 行内只放图标动作；完整可执行动作集在详情栏带文字展示。
            ForEach(ItemActions.forItem(appState: appState, item: item, compact: true)) { action in
                Button(action: action.run) {
                    SizqoIcon(name: action.icon, size: 13)
                }
                .buttonStyle(RowActionButtonStyle())
                .disabled(action.disabled)
                .opacity(action.disabled ? 0.45 : 1)
                .help(action.blockedReason ?? action.title)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .frame(minHeight: 40)
        .background(
            RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                .fill(rowBackground)
        )
        .overlay(
            RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                .strokeBorder(selected ? Color.accentColor : Color.clear, lineWidth: 1)
        )
        .opacity(rowOpacity)
        .contentShape(Rectangle())
        .onTapGesture { appState.selectedPath = item.path }
    }

    private var selected: Bool { appState.selectedPath == item.path }

    @ViewBuilder
    private var iconDisc: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(SizqoPalette.emphasisSoft)
                .frame(width: 24, height: 24)
            switch item.status {
            case .pending:
                SizqoIcon(
                    name: appState.compressionPaused ? SizqoIconName.pause : SizqoIconName.image,
                    size: 13, color: .secondary
                )
            case .running:
                ProgressView()
                    .scaleEffect(0.5)
                    .frame(width: 24, height: 24)
            case .done:
                // §69 图形与措辞同源：没产物或产物没达标都挂三角，绿勾只留给真有产物的完成。
                if item.result?.contractShowsWarning == true {
                    SizqoIcon(name: SizqoIconName.warning, size: 13, color: .yellow)
                } else {
                    SizqoIcon(name: SizqoIconName.check, size: 13, color: .green)
                }
            case .failed:
                SizqoIcon(name: SizqoIconName.close, size: 13, color: .red)
            case .removed:
                SizqoIcon(name: SizqoIconName.minus, size: 13, color: .secondary)
            case .restored:
                SizqoIcon(name: SizqoIconName.restore, size: 12, color: .secondary)
            }
        }
    }

    private var sizeText: String {
        if item.status == .done, let r = item.result, r.hasProduct {
            return "\(r.originalSizeFormatted) → \(r.compressedSizeFormatted)"
        }
        // 没产物时报的是原图大小 —— 那串从没写出去的数字不是这一行的事实。
        return CompressResult.formatBytesJS(item.fileSize)
    }

    private var rowBackground: Color {
        switch item.status {
        case .running: return SizqoPalette.accentSoft
        // §69 「有这一项但没产物」不是好消息，不许铺成功的底色。
        case .done: return item.result?.contractShowsWarning == true
            ? SizqoPalette.caution.opacity(0.10) : SizqoPalette.success.opacity(0.08)
        case .failed: return SizqoPalette.danger.opacity(0.08)
        default: return selected ? SizqoPalette.emphasisSoft : Color.clear
        }
    }

    private var rowOpacity: Double {
        switch item.status {
        case .pending: return 0.8
        case .removed: return 0.4
        case .restored: return 0.7
        default: return 1
        }
    }

    private var statusText: String {
        switch item.status {
        // 与后端阶段同源：暂停 ≠ 没在收尾，停止 ≠ 队列干完（见 state-machine.md）。
        case .pending: return appState.compressionPaused ? "已暂停" : "等待中"
        case .running:
            if appState.compressionPaused || appState.compressionStopping { return "收尾中…" }
            return appState.options.processingMode == .system ? "转换中…" : "压缩中…"
        case .done:
            guard let r = item.result else { return "完成" }
            // §70 「已完成」由绿色对勾表达，行上留降幅那个读数；结论不是"完成"的
            // （已跳过 / 未达目标 / 未产出文件）必须把结论词本身写出来。
            let status = r.contractStatusText
            if status == "已完成" { return r.savingsSignedText }
            return status
        case .failed: return item.result?.contractStatusText ?? "处理失败"
        case .removed: return "已移除"
        case .restored: return "已恢复"
        }
    }

    private var statusColor: Color {
        switch item.status {
        case .pending, .removed, .restored: return SizqoPalette.tertiaryText
        case .running: return .accentColor
        // §69 红色只留给真的坏了；三角留给"这一项没有产物 / 产物没达标"。
        case .done: return item.result?.contractShowsWarning == true ? .yellow : .green
        case .failed: return .red
        }
    }
}

// MARK: - 单件动作集（行内图标与详情栏共用同一判据，只列此刻真能做的）

struct ItemAction: Identifiable {
    let id = UUID()
    let icon: String
    let title: String
    var disabled = false
    var blockedReason: String? = nil
    let run: () -> Void
}

@MainActor
enum ItemActions {
    /// compact = 行内（不重复放「移除」以外的低频动作时保持密度）；详情栏列全集。
    static func forItem(appState: AppState, item: QueueItem, compact: Bool) -> [ItemAction] {
        var out: [ItemAction] = []
        switch item.status {
        case .pending:
            out.append(ItemAction(icon: SizqoIconName.close, title: "移除",
                                  run: { appState.removeItem(path: item.path) }))
        case .done:
            if let r = item.result {
                // §8 §68 按钮集只由结果契约回答一次：没有产物 = 指向那个文件的按钮一个都不许出现，
                // 而不是"点了再报错"。以前这里读 r.success，于是"没落盘的成功"照样给
                // 另存为 / 对比 / 恢复，按下去换来一句「找不到这条历史记录」。
                let caps = r.capabilities
                if caps.save {
                    out.append(ItemAction(icon: SizqoIconName.save, title: "另存为",
                                          run: { appState.saveResult(path: item.path) }))
                }
                if caps.compare {
                    out.append(ItemAction(icon: SizqoIconName.compare, title: "对比查看",
                                          run: {
                        CompareWindowController.show(
                            appState: appState, originalPath: item.path, result: r,
                            allResults: appState.comparableResults)
                    }))
                }
                if caps.restore {
                    // §44 会话期间只禁用不隐藏：按钮消失会让人以为这项没有原图可恢复，
                    // 说明写在 help 里，告诉用户这是时间问题。
                    let blocked = appState.restoreBlockedBySession
                    out.append(ItemAction(icon: SizqoIconName.restore, title: "恢复原图",
                                          disabled: blocked,
                                          blockedReason: blocked ? appState.restoreBlockedText : nil,
                                          run: { appState.restoreFile(path: item.path) }))
                }
                if caps.finder {
                    out.append(ItemAction(icon: SizqoIconName.finder, title: "在访达中显示",
                                          run: { appState.openInFinder(path: r.outputPath ?? item.path) }))
                }
                if caps.retry {
                    out.append(ItemAction(icon: SizqoIconName.recompress, title: "重新压缩",
                                          run: { appState.retryFile(path: item.path) }))
                }
                if caps.log {
                    out.append(ItemAction(icon: SizqoIconName.copy, title: "复制日志",
                                          run: { appState.copyLog(path: item.path) }))
                }
            }
        case .failed:
            out.append(ItemAction(icon: SizqoIconName.recompress, title: "重试",
                                  run: { appState.retryFile(path: item.path) }))
            out.append(ItemAction(icon: SizqoIconName.copy, title: "复制日志",
                                  run: { appState.copyLog(path: item.path) }))
        case .restored:
            out.append(ItemAction(icon: SizqoIconName.recompress, title: "重新压缩",
                                  run: { appState.retryFile(path: item.path) }))
        case .running, .removed:
            break
        }
        // compact 时行宽有限：done 行保留 恢复/对比/访达/日志 之外的都收进详情栏
        if compact, item.status == .done, item.result?.hasProduct == true {
            out = out.filter { $0.title != "另存为" }
        }
        return out
    }
}

// MARK: - 详情栏（选中项 = 单件详情 + 可用动作；空态 = 本轮汇总 + 当前意图摘要）

struct DetailColumnView: View {
    @EnvironmentObject var appState: AppState

    private var selectedItem: QueueItem? {
        guard let p = appState.selectedPath else { return nil }
        return appState.items.first { $0.path == p && $0.status != .removed }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 5) {
                SizqoIcon(name: SizqoIconName.eye, size: 14, color: .accentColor)
                Text(selectedItem == nil ? "本轮总览" : "详情")
                    .font(SizqoType.control)
                Spacer()
                if let item = selectedItem {
                    Text(item.fileName)
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
                    if let item = selectedItem {
                        ItemDetailView(item: item)
                    } else {
                        RunTotalsView()
                        ActiveIntentSummaryView()
                    }
                }
                .padding(SizqoSpacing.md)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .background(SizqoPalette.window)
    }
}

private struct ItemDetailView: View {
    @EnvironmentObject var appState: AppState
    let item: QueueItem

    var body: some View {
        VStack(alignment: .leading, spacing: SizqoSpacing.md) {
            preview
            rows
            if let r = item.result, r.targetBytes != nil {
                targetVerdict(r)
            }
            warningsBlock
            actionsBlock
        }
    }

    @ViewBuilder private var preview: some View {
        // 预览取的是**真在盘上的那个文件**：没产物时 outputPath 是空的，
        // 拿它去 LoadFile 会把原图当成"压缩结果"预览。
        let product = (item.result?.hasProduct == true) ? (item.result?.outputPath ?? "") : ""
        let path = product.isEmpty ? item.path : product
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
                    Text("无法读取预览")
                        .font(SizqoType.caption)
                        .foregroundColor(.secondary)
                )
        }
    }

    private var rows: some View {
        VStack(alignment: .leading, spacing: 5) {
            if item.status == .done, let r = item.result, r.hasProduct {
                detailRow("体积", "\(CompressResult.formatBytesJS(r.originalSize)) → \(CompressResult.formatBytesJS(r.compressedSize))")
                detailRow("节省", r.savingsSignedText, r.savings >= 0 ? .green : .orange)
                detailRow("尺寸", dimensionsText(r))
                detailRow("算法", r.algorithm.isEmpty ? "(未知)" : r.algorithm)
                if let ms = r.elapsedMs { detailRow("耗时", "\(ms) ms") }
                detailRow("输出格式", r.outType.isEmpty ? "(未知)" : r.outType)
                if let out = r.outputPath, !out.isEmpty { detailRow("输出位置", out) }
            } else {
                // §6.1 没产物就没有"压缩后大小"这回事：那一串数字从没写出去过。
                detailRow("体积", CompressResult.formatBytesJS(item.fileSize))
                detailRow("状态", statusLine)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var statusLine: String {
        switch item.status {
        case .pending: return appState.compressionPaused ? "已暂停（等待中的文件不会前进）" : "等待中"
        case .running: return appState.compressionStopping ? "收尾中…" : "压缩中…"
        // §69 详情栏与行上是同一个解释器：结论 + 为什么，不再各读一遍旗标。
        case .done: return item.result?.contractDetailText ?? "—"
        case .failed:
            let why = item.result?.contractDetailText ?? ""
            return why.isEmpty ? "处理失败" : why
        default: return "—"
        }
    }

    private func dimensionsText(_ r: CompressResult) -> String {
        let src = r.sourceWidth != nil ? "\(r.sourceWidth!)×\(r.sourceHeight!)" : "(未知)"
        let out = r.outputWidth != nil ? "\(r.outputWidth!)×\(r.outputHeight!)" : "(未知)"
        return src == out ? src : "\(src) → \(out)"
    }

    private func targetVerdict(_ r: CompressResult) -> some View {
        let met = r.targetMet == true
        return HStack(alignment: .top, spacing: 6) {
            SizqoIcon(name: SizqoIconName.targetSize, size: 13, color: met ? .green : .orange)
            VStack(alignment: .leading, spacing: 2) {
                Text("体积预算 \(CompressResult.formatBytesJS(r.targetBytes ?? 0))")
                    .font(SizqoType.captionMedium)
                // 诚实规则 1：没打中预算绝不当成达成。
                Text(met ? "已达成" : "未达成 · 实际 \(CompressResult.formatBytesJS(r.compressedSize))（最接近的一次）")
                    .font(SizqoType.caption)
                    .foregroundColor(met ? .green : .orange)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: SizqoRadius.control, style: .continuous)
                .fill((met ? Color.green : Color.orange).opacity(0.08))
        )
    }

    @ViewBuilder private var warningsBlock: some View {
        let warnings = (item.result?.warnings ?? [])
            .compactMap { $0 as String? }
        if !warnings.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(warnings.enumerated()), id: \.offset) { _, w in
                    HStack(alignment: .top, spacing: 5) {
                        SizqoIcon(name: SizqoIconName.warning, size: 11, color: .yellow)
                            .padding(.top, 1)
                        Text(w)
                            .font(SizqoType.caption)
                            .foregroundColor(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
        if let err = item.result?.error, !err.isEmpty, item.status == .failed {
            Text(err)
                .font(SizqoType.caption)
                .foregroundColor(SizqoPalette.danger)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var actionsBlock: some View {
        // 契约：只列这项此刻真能执行的动作（判据与行内完全同源）。
        let actions = ItemActions.forItem(appState: appState, item: item, compact: false)
        return VStack(alignment: .leading, spacing: 6) {
            if !actions.isEmpty {
                HairlineDivider()
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 130), spacing: 6)], alignment: .leading, spacing: 6) {
                    ForEach(actions) { action in
                        Button(action: action.run) {
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
                        .disabled(action.disabled)
                        .opacity(action.disabled ? 0.45 : 1)
                        .help(action.blockedReason ?? action.title)
                    }
                }
            }
        }
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
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}

private struct RunTotalsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("本轮")
                .font(SizqoType.captionMedium)
                .foregroundColor(.secondary)
            HStack(spacing: SizqoSpacing.lg) {
                stat("原始", CompressResult.formatBytesJS(appState.totalOriginal), .primary)
                stat("压缩后", CompressResult.formatBytesJS(appState.totalCompressed), .accentColor)
                stat("已节省", CompressResult.formatBytesJS(appState.totalSaved), .green)
                stat("压缩率", String(format: "%.1f%%", appState.totalSavingsPct), .green)
            }
            Text("\(appState.queueProgress.summaryText)")
                .font(SizqoType.caption)
                .foregroundColor(.secondary)
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

private struct ActiveIntentSummaryView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HairlineDivider()
            HStack(spacing: 5) {
                SizqoIcon(name: SizqoIconName.compress, size: 13, color: .accentColor)
                Text("当前意图 · \(appState.intent.label)")
                    .font(SizqoType.captionMedium)
            }
            Text(appState.activeIntentSummary)
                .font(SizqoType.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(appState.intent.helpText)
                .font(SizqoType.caption)
                .foregroundColor(SizqoPalette.tertiaryText)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

// MARK: - 运行条（全宽常驻；文案全部派生自后端会话阶段，不看按钮字面）

struct RunBarView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        HStack(spacing: SizqoSpacing.md) {
            CompressButton()
                .frame(width: 180)

            pauseStopCluster

            VStack(alignment: .leading, spacing: 2) {
                progressFill
                HStack(spacing: SizqoSpacing.lg) {
                    Text(appState.queueProgress.summaryText)
                        .font(SizqoType.caption.monospacedDigit())
                        .foregroundColor(.secondary)
                    Text("已省 \(CompressResult.formatBytesJS(appState.totalSaved)) · \(String(format: "%.1f", appState.totalSavingsPct))%")
                        .font(SizqoType.caption.monospacedDigit())
                        .foregroundColor(.green)
                    Text(appState.cpuSummaryText)
                        .font(SizqoType.caption)
                        .foregroundColor(.secondary)
                    Text(appState.compressionPhaseSummary.trimmingCharacters(in: CharacterSet(charactersIn: " ·")))
                        .font(SizqoType.captionMedium)
                        .foregroundColor(SizqoPalette.running)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, SizqoSpacing.rowX)
        .padding(.vertical, 8)
        .background(SizqoPalette.window)
    }

    private var progressFill: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(SizqoPalette.emphasisSoft).frame(height: 4)
                Capsule()
                    .fill(appState.failedCount > 0 ? Color.orange : Color.accentColor)
                    .frame(width: max(0, geo.size.width * appState.compressProgress), height: 4)
                    .animation(.easeOut(duration: 0.3), value: appState.compressProgress)
            }
        }
        .frame(height: 4)
    }

    @ViewBuilder private var pauseStopCluster: some View {
        if appState.isCompressing {
            Button {
                appState.togglePause()
            } label: {
                Label {
                    Text(appState.compressionPaused ? "继续" : "暂停")
                } icon: {
                    SizqoIcon(
                        name: appState.compressionPaused ? SizqoIconName.play : SizqoIconName.pause,
                        size: 14
                    )
                }
                .labelIconToTextSpacing(3)
            }
            .buttonStyle(SecondaryButtonStyle())
            .disabled(appState.compressionStopping)
            .help(appState.compressionPaused ? "继续压缩" : "暂停：正在压缩的文件会先完成，之后再开新的")

            Button {
                appState.stopBatch()
            } label: {
                Label {
                    Text(appState.compressionStopping ? "正在停止…" : "停止")
                } icon: {
                    SizqoIcon(name: SizqoIconName.stop, size: 14)
                }
                .labelIconToTextSpacing(3)
            }
            .buttonStyle(SecondaryButtonStyle())
            .disabled(appState.compressionStopping)
            .help("停止：正在处理的文件会先完成，其余文件保留在队列中")
        }
    }
}

// MARK: - Compress Button（.compress-progress-btn 语义保留：文案 = AppState 队列派生）

struct CompressButton: View {
    @EnvironmentObject var appState: AppState

    /// 文案由 AppState 按**队列**算（还剩 pending → 继续压缩；全压完 → 完成）。
    /// 这里只负责画，不在视图里另算一套。
    private var buttonText: String { appState.compressButtonText }

    private var buttonIcon: String {
        if appState.isCompressing {
            return appState.compressionPhase == .paused
                ? SizqoIconName.pause
                : SizqoIconName.recompress
        }
        if isDone { return SizqoIconName.check }
        if appState.pendingCount > 0 && appState.processedCount > 0 { return SizqoIconName.play }
        return SizqoIconName.compress
    }

    var body: some View {
        Button {
            appState.startCompress()
        } label: {
            ZStack(alignment: .leading) {
                GeometryReader { geo in
                    Rectangle()
                        .fill(fillColor)
                        .frame(width: appState.isCompressing || appState.processedCount > 0
                               ? geo.size.width * appState.compressProgress
                               : 0)
                        .animation(.easeOut(duration: 0.3), value: appState.compressProgress)
                }
                HStack(spacing: 6) {
                    SizqoIcon(name: buttonIcon, size: 15)
                    Text(buttonText)
                }
                .font(.system(size: 13, weight: .semibold))
                .frame(maxWidth: .infinity)
            }
            .padding(.vertical, 8)
            .padding(.horizontal, 14)
            .frame(maxWidth: .infinity)
            .background(
                RoundedRectangle(cornerRadius: AppMetrics.smallRadius, style: .continuous)
                    .fill(Color.accentColor)
            )
            .foregroundColor(isDone ? .green : .white)
            .opacity(appState.pendingCount == 0 && !appState.isCompressing ? 0.4 : 1)
            .contentShape(RoundedRectangle(cornerRadius: AppMetrics.smallRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(appState.isCompressing || appState.pendingCount == 0)
        .animation(.easeOut(duration: 0.2), value: appState.compressButtonText)
    }

    /// 全队列处理完（没有待处理的）才算"完成态"：停止之后还剩 pending，
    /// 这时候按钮必须还是「继续压缩」，不能显示成绿的完成。
    private var isDone: Bool {
        !appState.isCompressing
            && appState.pendingCount == 0
            && appState.processedCount > 0
            && appState.failedCount == 0
    }

    private var fillColor: Color {
        if isDone { return Color.green.opacity(0.35) }
        return Color.white.opacity(0.18)
    }
}

// MARK: - System Info Modal

struct SystemInfoModal: View {
    @Binding var isPresented: Bool

    var body: some View {
        ZStack {
            Color.black.opacity(0.3)
                .onTapGesture { isPresented = false }

            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 6) {
                    SizqoIcon(name: SizqoIconName.info, size: 15, color: .accentColor)
                    Text("系统转换说明")
                        .font(.system(size: 14, weight: .semibold))
                    Spacer()
                    Button {
                        isPresented = false
                    } label: {
                        SizqoIcon(name: SizqoIconName.error, size: 16, color: .secondary.opacity(0.4))
                    }
                    .buttonStyle(.plain)
                }

                Text("系统转换使用 macOS 自带的图像转换能力，行为对齐 Finder 中选中图片后右键「快速操作 → 转换图像」的默认方式。")
                    .font(SizqoType.body)
                    .fixedSize(horizontal: false, vertical: true)

                Text("可选择输出格式、图像大小和是否保留元数据；不会调用第三方压缩工具。此模式仅在 macOS 上可用。")
                    .font(SizqoType.body)
                    .fixedSize(horizontal: false, vertical: true)

                HStack {
                    Spacer()
                    Button("知道了") { isPresented = false }
                        .buttonStyle(PrimaryButtonStyle())
                }
                .padding(.top, 2)
            }
            .padding(16)
            .frame(width: 400)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(SizqoPalette.window)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(SizqoPalette.hairline, lineWidth: 0.5)
            )
            .shadow(color: Color.black.opacity(0.25), radius: 16, y: 4)
            .padding(24)
        }
    }
}

// MARK: - 小工具：Label 图标与文字间距

extension Label {
    /// 精确控制图标与文字间距（对应 .btn 的 gap: 5px）
    func labelIconToTextSpacing(_ spacing: CGFloat) -> some View {
        environment(\.symbolRenderingMode, .monochrome)
            // SwiftUI Label 原生间距约为 4-6，这里通过字体微调保持紧凑
            .font(.system(size: 12, weight: .medium))
    }
}


// MARK: - §3.4 人工恢复现场横幅

/// 把"当前文件 vs 原图备份"的状态摆出来，恢复必须拿到用户确认；
/// 暂不处理 = 现场原样保留，随时可以再决定。
struct ManualRecoveryBanner: View {
    let items: [OutputTransactionStore.PendingRecoveryItem]
    let onResolve: (OutputTransactionStore.PendingRecoveryItem, Bool, Bool) -> Void
    @State private var confirming: OutputTransactionStore.PendingRecoveryItem?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("检测到需要人工处理的恢复现场")
                    .font(.headline)
                Spacer()
                Text(String(format: "共 %d 个现场待处理", items.count))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            ForEach(items, id: \.historyId) { item in
                VStack(alignment: .leading, spacing: 4) {
                    Text(item.detail).font(.subheadline)
                    Text("当前文件: \(item.sourcePath)（\(item.sourceExists ? "存在，\(item.sourceSize) 字节" : "不存在")）")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("原图备份: \(item.backupPath)（\(item.backupExists ? "存在，\(item.backupSize) 字节" : "已丢失")）")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if item.voucherMatch == false {
                        Text("警告：备份字节与记账时的指纹不符")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    if item.status == .legacyUnknown {
                        Text("备份未经真实性验证：请确认来历后再决定是否写回")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    HStack {
                        Button("恢复原图") { confirming = item }
                            .buttonStyle(.borderedProminent)
                        Button("暂不处理") { onResolve(item, false, false) }
                        Spacer()
                    }
                }
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 10).fill(Color.secondary.opacity(0.08)))
            }
        }
        .padding(12)
        .alert("恢复原图会覆盖当前位置的文件", isPresented: Binding(
            get: { confirming != nil },
            set: { if !$0 { confirming = nil } }
        )) {
            Button("仍要恢复", role: .destructive) {
                if let item = confirming {
                    // 凭证不符 = 用户看过警告后明确放行（与 Rust/前端同一协议）。
                    onResolve(item, true, item.voucherMatch == false)
                }
                confirming = nil
            }
            Button("取消", role: .cancel) { confirming = nil }
        } message: {
            Text(confirming.map { item in
                "当前文件: \(item.sourcePath)\n将写回的原图备份: \(item.backupPath)"
                    + (item.voucherMatch == false
                        ? "\n\n警告：这份备份的字节与记账时的指纹不符，恢复的内容可能不是当初的原图。继续即表示你了解并接受这一风险。"
                        : "")
            } ?? "")
        }
    }
}
