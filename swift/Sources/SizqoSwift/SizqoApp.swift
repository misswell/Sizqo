import SwiftUI
import AppKit

@available(macOS 13.0, *)
@main
struct SizqoApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var appState = AppState()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(appState)
                // 三列布局（rail 76 + 工作列 + 详情列 340）要求更宽的窗口；
                // 与 Tauri 线 tauri.conf.json 同一组数：1180×760，最小 940×620。
                .frame(minWidth: 940, minHeight: 620)
        }
        .defaultSize(width: 1180, height: 760)
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .appInfo) { }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        // 启动时清掉上次崩溃残留的临时目录（正常退出时也清理一次）
        cleanupTempDirs()
        // 主窗口关闭即退出（与 Tauri 的 on_window_event 行为一致），
        // 但关闭对比窗口不应退出。
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(windowWillClose(_:)),
            name: NSWindow.willCloseNotification,
            object: nil
        )
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationWillTerminate(_ notification: Notification) {
        cleanupTempDirs()
        applyRetentionOnExit()
    }

    /// 保留档位唯一的执行点：此刻的档位就是用户最终选的那个。
    ///
    /// 只挂在正常退出上，启动时不清 —— 崩溃 / 强杀现场的那份备份可能是唯一还活着的
    /// 原图。异常退出留下的那批由 `previousRunEndedCleanly` 认出来，再留一个会话。
    private func applyRetentionOnExit() {
        // 必须用进程内那一份实例：损坏锁死标记挂在实例上，另 new 一个会把启动时
        // 立起来的锁丢掉，现场刚留档就被 sweep。
        // 与 clearHistory 同一道闸门判据：批次还在跑或事务未收口时，备份目录可能
        // 刚建好、还没写进历史（rename 之后、add 之前），sweep 会把它当孤儿删掉 ——
        // 而它可能正是那份原图唯一的副本。跳过本次清理，下次退出再收账；
        // 不留「已清账」记号，下次运行继续按"上次未清账"对待。
        guard AppState.exitCleanupPermitted() else {
            NSLog("Sizqo 退出时批次/事务仍在收尾，跳过保留档位清理，原图备份全部保留")
            return
        }
        let store = HistoryStore.shared
        let report = store.applyRetentionOnExit(
            retentionDays: SettingsStore().load().originalRetentionDays,
            runStartedAt: store.runStartedAt,
            previousRunEndedCleanly: store.previousRunEndedCleanly
        )
        if report.removedEntries > 0 || report.removedBackups > 0 {
            NSLog("Sizqo 退出清理: 历史记录 -%d 条，原图备份 -%d 份，保留 %d 份",
                  report.removedEntries, report.removedBackups, report.keptBackups)
        }
        for warning in report.warnings { NSLog("Sizqo 退出清理未完成: %@", warning) }
        // 只有账真结了才留记号。历史不可信或写盘失败时跳过清理，那批备份就得继续留着，
        // 于是下一次退出仍然按"上次异常退出"处理，多给它一个会话的机会。
        if report.warnings.isEmpty { store.markCleanExit() }
    }

    @objc private func windowWillClose(_ notification: Notification) {
        guard let win = notification.object as? NSWindow else { return }
        // 仅主窗口（identifier 由 ContentView.onAppear 设置）关闭时退出，
        // 关闭对比窗口 / 弹窗不退出。
        if win.identifier?.rawValue == "main" {
            NSApp.terminate(nil)
        }
    }

    /// 删除临时目录：重压缩对比文件、压缩中间产物。
    /// 原图备份在 App Support 下，去留由保留档位决定（见 applyRetentionOnExit），
    /// 这里绝不能再删；sizqo-backups 只是旧版本留下的临时备份残骸，顺手清掉。
    private func cleanupTempDirs() {
        let fm = FileManager.default
        for name in ["sizqo-backups", "sizqo-display", "sizqo-work"] {
            try? fm.removeItem(atPath: NSTemporaryDirectory() + name)
        }
    }
}
