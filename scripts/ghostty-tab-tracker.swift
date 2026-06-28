#!/usr/bin/env swift

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

let args = Array(CommandLine.arguments.dropFirst())
let outputJSON = args.contains("--json")
let bundleID = args.first(where: { !$0.hasPrefix("--") }) ?? "com.mitchellh.ghostty"
let pollInterval: TimeInterval = 0.75
let frameTolerance: CGFloat = 3

@_silgen_name("_AXUIElementGetWindow")
func _AXUIElementGetWindow(_ element: AXUIElement, _ identifier: UnsafeMutablePointer<CGWindowID>) -> AXError

struct AXWindowSnapshot: Codable, Equatable {
    let axID: String
    let windowID: Int?
    let axIndex: Int
    let title: String
    let role: String
    let subrole: String
    let focused: Bool
    let main: Bool
    let minimized: Bool
    let frame: RectSnapshot?
    let nativeTabGroupID: String?
    let nativeTabIndex: Int?
    let nativeTabFront: Bool
}

struct CGWindowSnapshot: Codable, Equatable {
    let windowID: Int
    let title: String
    let frame: RectSnapshot
}

struct NativeTabGroupSnapshot: Codable, Equatable {
    let groupID: String
    let frame: RectSnapshot?
    let frontAXID: String?
    let orderedAXIDs: [String]
    let titles: [String]
}

struct Snapshot: Codable, Equatable {
    let reason: String
    let timestamp: String
    let pid: Int32
    let appName: String
    let axWindows: [AXWindowSnapshot]
    let cgOnScreenWindows: [CGWindowSnapshot]
    let nativeTabGroups: [NativeTabGroupSnapshot]
}

struct RectSnapshot: Codable, Equatable, Hashable {
    let x: Int
    let y: Int
    let w: Int
    let h: Int

    init(_ rect: CGRect) {
        x = Int(rect.origin.x.rounded())
        y = Int(rect.origin.y.rounded())
        w = Int(rect.size.width.rounded())
        h = Int(rect.size.height.rounded())
    }

    init(point: CGPoint, size: CGSize) {
        x = Int(point.x.rounded())
        y = Int(point.y.rounded())
        w = Int(size.width.rounded())
        h = Int(size.height.rounded())
    }

    var key: String { "\(x):\(y):\(w):\(h)" }
}

func nowString() -> String {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return formatter.string(from: Date())
}

func axString(_ element: AXUIElement, _ attr: CFString) -> String? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attr, &value) == .success else { return nil }
    return value as? String
}

func axBool(_ element: AXUIElement, _ attr: CFString) -> Bool? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attr, &value) == .success else { return nil }
    return value as? Bool
}

func axPoint(_ element: AXUIElement, _ attr: CFString) -> CGPoint? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attr, &value) == .success,
          let axValue = value as! AXValue?,
          AXValueGetType(axValue) == .cgPoint else { return nil }
    var point = CGPoint.zero
    AXValueGetValue(axValue, .cgPoint, &point)
    return point
}

func axSize(_ element: AXUIElement, _ attr: CFString) -> CGSize? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attr, &value) == .success,
          let axValue = value as! AXValue?,
          AXValueGetType(axValue) == .cgSize else { return nil }
    var size = CGSize.zero
    AXValueGetValue(axValue, .cgSize, &size)
    return size
}

func axArray(_ element: AXUIElement, _ attr: CFString) -> [AXUIElement] {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attr, &value) == .success else { return [] }
    return (value as? [AXUIElement]) ?? []
}

func cgWindows(pid: pid_t) -> [CGWindowSnapshot] {
    let raw = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    return raw.compactMap { info in
        guard (info[kCGWindowOwnerPID as String] as? pid_t) == pid,
              let id = info[kCGWindowNumber as String] as? Int,
              let bounds = info[kCGWindowBounds as String] as? [String: Any],
              let x = bounds["X"] as? CGFloat,
              let y = bounds["Y"] as? CGFloat,
              let w = bounds["Width"] as? CGFloat,
              let h = bounds["Height"] as? CGFloat else { return nil }
        return CGWindowSnapshot(
            windowID: id,
            title: (info[kCGWindowName as String] as? String) ?? "",
            frame: RectSnapshot(CGRect(x: x, y: y, width: w, height: h))
        )
    }
}

func closeEnough(_ lhs: RectSnapshot?, _ rhs: RectSnapshot?) -> Bool {
    guard let lhs, let rhs else { return false }
    return abs(lhs.x - rhs.x) <= Int(frameTolerance)
        && abs(lhs.y - rhs.y) <= Int(frameTolerance)
        && abs(lhs.w - rhs.w) <= Int(frameTolerance)
        && abs(lhs.h - rhs.h) <= Int(frameTolerance)
}

func frameGroupKey(_ frame: RectSnapshot?) -> String {
    guard let frame else { return "missing-frame" }
    return frame.key
}

func inferFrontAXID(group: [PartialAXWindow], cg: [CGWindowSnapshot]) -> String? {
    if let focused = group.first(where: { $0.focused || $0.main }) {
        return focused.axID
    }

    let matchingCG = cg.filter { cgWindow in
        group.contains { closeEnough($0.frame, cgWindow.frame) }
    }

    for cgWindow in matchingCG {
        let byTitle = group.filter { !$0.title.isEmpty && $0.title == cgWindow.title }
        if byTitle.count == 1 {
            return byTitle[0].axID
        }
    }

    // If CoreGraphics sees exactly one window at this frame but title matching is
    // ambiguous/missing, fall back to the first AX window in the group. This is
    // marked by ordering only; inspect logs before relying on it as native order.
    if matchingCG.count == 1 {
        return group.first?.axID
    }

    return group.first?.axID
}

struct PartialAXWindow: Equatable {
    let element: AXUIElement
    let axID: String
    let windowID: Int?
    let axIndex: Int
    let title: String
    let role: String
    let subrole: String
    let focused: Bool
    let main: Bool
    let minimized: Bool
    let frame: RectSnapshot?
}

func axWindowID(_ element: AXUIElement) -> Int? {
    var windowID = CGWindowID(0)
    guard _AXUIElementGetWindow(element, &windowID) == .success, windowID != 0 else {
        return nil
    }
    return Int(windowID)
}

func stableAXID(_ element: AXUIElement) -> (String, Int?) {
    if let windowID = axWindowID(element) {
        return ("win:\(windowID)", windowID)
    }
    return ("ax:\(CFHash(element))", nil)
}

func takeSnapshot(app: NSRunningApplication, reason: String) -> Snapshot {
    let pid = app.processIdentifier
    let axApp = AXUIElementCreateApplication(pid)
    let axWindows = axArray(axApp, kAXWindowsAttribute as CFString)

    let partials = axWindows.enumerated().map { index, element in
        let point = axPoint(element, kAXPositionAttribute as CFString)
        let size = axSize(element, kAXSizeAttribute as CFString)
        let identity = stableAXID(element)
        return PartialAXWindow(
            element: element,
            axID: identity.0,
            windowID: identity.1,
            axIndex: index,
            title: axString(element, kAXTitleAttribute as CFString) ?? "",
            role: axString(element, kAXRoleAttribute as CFString) ?? "",
            subrole: axString(element, kAXSubroleAttribute as CFString) ?? "",
            focused: axBool(element, kAXFocusedAttribute as CFString) ?? false,
            main: axBool(element, kAXMainAttribute as CFString) ?? false,
            minimized: axBool(element, kAXMinimizedAttribute as CFString) ?? false,
            frame: point.flatMap { point in size.map { RectSnapshot(point: point, size: $0) } }
        )
    }

    let cg = cgWindows(pid: pid)

    let grouped = Dictionary(grouping: partials) { frameGroupKey($0.frame) }
    let nativeGroups = grouped.values
        .filter { $0.count > 1 }
        .map { group -> NativeTabGroupSnapshot in
            let ordered = group.sorted { $0.axIndex < $1.axIndex }
            let front = inferFrontAXID(group: ordered, cg: cg)
            return NativeTabGroupSnapshot(
                groupID: ordered.map(\.axID).joined(separator: ":"),
                frame: ordered.first?.frame,
                frontAXID: front,
                orderedAXIDs: ordered.map(\.axID),
                titles: ordered.map(\.title)
            )
        }
        .sorted { ($0.frame?.key ?? $0.groupID) < ($1.frame?.key ?? $1.groupID) }

    let groupByAXID: [String: NativeTabGroupSnapshot] = Dictionary(
        uniqueKeysWithValues: nativeGroups.flatMap { group in
            group.orderedAXIDs.map { ($0, group) }
        }
    )

    let windows = partials.map { win -> AXWindowSnapshot in
        let group = groupByAXID[win.axID]
        return AXWindowSnapshot(
            axID: win.axID,
            windowID: win.windowID,
            axIndex: win.axIndex,
            title: win.title,
            role: win.role,
            subrole: win.subrole,
            focused: win.focused,
            main: win.main,
            minimized: win.minimized,
            frame: win.frame,
            nativeTabGroupID: group?.groupID,
            nativeTabIndex: group?.orderedAXIDs.firstIndex(of: win.axID),
            nativeTabFront: group?.frontAXID == win.axID
        )
    }

    return Snapshot(
        reason: reason,
        timestamp: nowString(),
        pid: pid,
        appName: app.localizedName ?? bundleID,
        axWindows: windows,
        cgOnScreenWindows: cg,
        nativeTabGroups: nativeGroups
    )
}

var labels: [String: String] = [:]
var nextLabel = 1

func label(for axID: String) -> String {
    if let existing = labels[axID] {
        return existing
    }
    let created = axID.hasPrefix("win:") ? "#\(axID.dropFirst(4))" : "W\(nextLabel)"
    nextLabel += 1
    labels[axID] = created
    return created
}

func shortTitle(_ value: String, limit: Int = 70) -> String {
    let oneLine = value.replacingOccurrences(of: "\n", with: " ")
    if oneLine.count <= limit {
        return oneLine
    }
    return String(oneLine.prefix(limit - 1)) + "…"
}

func rectText(_ rect: RectSnapshot?) -> String {
    guard let rect else { return "no-frame" }
    return "\(rect.x),\(rect.y) \(rect.w)x\(rect.h)"
}

func jsonPrint(_ snapshot: Snapshot) {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    if let data = try? encoder.encode(snapshot), let text = String(data: data, encoding: .utf8) {
        print(text)
    }
}

func printDiff(_ snapshot: Snapshot, previous: Snapshot?) {
    guard let previous else { return }

    let oldByID = Dictionary(uniqueKeysWithValues: previous.axWindows.map { ($0.axID, $0) })
    let newByID = Dictionary(uniqueKeysWithValues: snapshot.axWindows.map { ($0.axID, $0) })
    let oldIDs = Set(oldByID.keys)
    let newIDs = Set(newByID.keys)

    let added = newIDs.subtracting(oldIDs).sorted()
    let removed = oldIDs.subtracting(newIDs).sorted()
    for id in added {
        if let win = newByID[id] {
            print("+ \(label(for: id)) \"\(shortTitle(win.title))\" frame=\(rectText(win.frame))")
        }
    }
    for id in removed {
        print("- \(label(for: id))")
    }

    for id in oldIDs.intersection(newIDs).sorted() {
        guard let old = oldByID[id], let new = newByID[id] else { continue }
        var changes: [String] = []
        if old.axIndex != new.axIndex { changes.append("AX index \(old.axIndex)->\(new.axIndex)") }
        if old.focused != new.focused { changes.append(new.focused ? "focused" : "unfocused") }
        if old.main != new.main { changes.append(new.main ? "main" : "not-main") }
        if old.nativeTabFront != new.nativeTabFront { changes.append(new.nativeTabFront ? "front-tab" : "hidden-tab") }
        if old.nativeTabGroupID != new.nativeTabGroupID { changes.append("group changed") }
        if old.title != new.title { changes.append("title=\"\(shortTitle(new.title))\"") }
        if !changes.isEmpty {
            print("~ \(label(for: id)) \(changes.joined(separator: ", "))")
        }
    }

    for group in snapshot.nativeTabGroups {
        let newSet = Set(group.orderedAXIDs)
        let old = previous.nativeTabGroups.first { Set($0.orderedAXIDs) == newSet }
        if old == nil {
            print("+ group \(group.orderedAXIDs.map { label(for: $0) }.joined(separator: ","))")
            continue
        }
        if old?.orderedAXIDs != group.orderedAXIDs {
            print("~ group order: \(group.orderedAXIDs.map { label(for: $0) }.joined(separator: " → "))")
        }
        if old?.frontAXID != group.frontAXID {
            print("~ group front: \(old?.frontAXID.map { label(for: $0) } ?? "none") -> \(group.frontAXID.map { label(for: $0) } ?? "none")")
        }
    }
}

func printSnapshot(_ snapshot: Snapshot, previous: Snapshot?) {
    print("\n=== \(snapshot.timestamp) :: \(snapshot.reason) ===")
    print("\(snapshot.appName) pid=\(snapshot.pid)  AX=\(snapshot.axWindows.count)  CG-on-screen=\(snapshot.cgOnScreenWindows.count)  inferred-tab-groups=\(snapshot.nativeTabGroups.count)")
    printDiff(snapshot, previous: previous)

    if snapshot.nativeTabGroups.isEmpty {
        print("tabs: none inferred")
    } else {
        print("tabs:")
        for (groupIndex, group) in snapshot.nativeTabGroups.enumerated() {
            print("  G\(groupIndex + 1) frame=\(rectText(group.frame)) front=\(group.frontAXID.map { label(for: $0) } ?? "none")")
            for (index, id) in group.orderedAXIDs.enumerated() {
                let win = snapshot.axWindows.first { $0.axID == id }
                let marker = id == group.frontAXID ? "*" : " "
                print("    \(marker) [\(index)] \(label(for: id)) \"\(shortTitle(win?.title ?? ""))\"")
            }
        }
    }

    let groupedIDs = Set(snapshot.nativeTabGroups.flatMap(\.orderedAXIDs))
    let standalone = snapshot.axWindows.filter { !groupedIDs.contains($0.axID) }
    if !standalone.isEmpty {
        print("standalone:")
        for win in standalone.sorted(by: { $0.axIndex < $1.axIndex }) {
            let marker = win.focused || win.main ? "*" : " "
            print("  \(marker) [\(win.axIndex)] \(label(for: win.axID)) frame=\(rectText(win.frame)) \"\(shortTitle(win.title))\"")
        }
    }

    if outputJSON {
        print("json:")
        jsonPrint(snapshot)
    }
    fflush(stdout)
}

func findGhostty() -> NSRunningApplication? {
    NSWorkspace.shared.runningApplications.first { app in
        app.bundleIdentifier == bundleID || app.localizedName == bundleID
    }
}

let trusted = AXIsProcessTrustedWithOptions([
    kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true
] as CFDictionary)
if !trusted {
    fputs("Accessibility permission is required. Enable this terminal/program and re-run.\n", stderr)
    exit(2)
}

guard let app = findGhostty() else {
    fputs("Could not find running app matching \(bundleID). Start Ghostty first.\n", stderr)
    exit(1)
}

var latestApp = app
var latestSnapshot: Snapshot?

@Sendable
func sameTrackedState(_ lhs: Snapshot, _ rhs: Snapshot) -> Bool {
    lhs.pid == rhs.pid
        && lhs.appName == rhs.appName
        && lhs.axWindows == rhs.axWindows
        && lhs.cgOnScreenWindows == rhs.cgOnScreenWindows
        && lhs.nativeTabGroups == rhs.nativeTabGroups
}

func emit(_ reason: String) {
    if let refreshed = findGhostty() {
        latestApp = refreshed
    }
    let snapshot = takeSnapshot(app: latestApp, reason: reason)
    if latestSnapshot.map({ !sameTrackedState(snapshot, $0) }) ?? true {
        printSnapshot(snapshot, previous: latestSnapshot)
        latestSnapshot = snapshot
    }
}

var observer: AXObserver?
let err = AXObserverCreate(app.processIdentifier, { _, _, notification, _ in
    emit(notification as String)
}, &observer)

guard err == .success, let observer else {
    fputs("AXObserverCreate failed: \(err.rawValue)\n", stderr)
    exit(3)
}

let axApp = AXUIElementCreateApplication(app.processIdentifier)
let notifications: [CFString] = [
    kAXFocusedWindowChangedNotification as CFString,
    kAXWindowCreatedNotification as CFString,
    kAXWindowMovedNotification as CFString,
    kAXWindowResizedNotification as CFString,
    kAXTitleChangedNotification as CFString,
    kAXUIElementDestroyedNotification as CFString
]
for note in notifications {
    let addErr = AXObserverAddNotification(observer, axApp, note, nil)
    if addErr != .success {
        fputs("Could not observe \(note): \(addErr.rawValue)\n", stderr)
    }
}

print("Watching \(latestApp.localizedName ?? bundleID) pid=\(latestApp.processIdentifier) bundle=\(latestApp.bundleIdentifier ?? "")")
print("Try: new windows, new native tabs, reorder tabs, move tabs between windows.")
print("Important: AX order may or may not be native tab-bar order. This script prints order changes so we can verify.")
emit("initial")

Timer.scheduledTimer(withTimeInterval: pollInterval, repeats: true) { _ in
    emit("poll")
}

CFRunLoopAddSource(CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(observer), .defaultMode)
CFRunLoopRun()
