import AppKit
import Charts
import Foundation
import SwiftUI
import UniformTypeIdentifiers

private enum PrototypeColor {
    static let accent = Color(nsColor: NSColor(red: 0.173, green: 0.431, blue: 0.286, alpha: 1))
    static let success = Color(nsColor: NSColor(red: 0.098, green: 0.463, blue: 0.239, alpha: 1))
    static let warning = Color(nsColor: NSColor(red: 0.541, green: 0.353, blue: 0, alpha: 1))
    static let danger = Color(nsColor: NSColor(red: 0.702, green: 0.149, blue: 0.118, alpha: 1))
}

enum NativeAsset {
    static func image(_ relativePath: String) -> NSImage? {
        guard let resources = Bundle.main.resourceURL else { return nil }
        return NSImage(contentsOf: resources.appendingPathComponent("Assets/").appendingPathComponent(relativePath))
    }
}

struct NativeBrandMark: View {
    var size: CGFloat

    var body: some View {
        Group {
            if let image = NativeAsset.image("brand/mark.svg") {
                Image(nsImage: image).resizable().scaledToFit()
            } else {
                Image(systemName: "shield.lefthalf.filled").resizable().scaledToFit()
            }
        }
        .frame(width: size, height: size)
        .shadow(color: .black.opacity(0.12), radius: 2, y: 1)
    }
}

struct PrototypeOverviewPage: View {
    @ObservedObject var store: RuntimeStore
    @Binding var selection: AppSection?
    let openProfiles: () -> Void
    @State private var selectedUsage: RequestActivity?
    @State private var showPrivacy = false
    @State private var showLocalAPI = false

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                PrototypeStatusSurface(
                    store: store,
                    openProfiles: openProfiles,
                    openPrivacy: { showPrivacy = true }
                )
                if let problem = store.state.endpointError ?? store.state.error {
                    Label(problem, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(PrototypeColor.danger)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(PrototypeColor.danger.opacity(0.09), in: RoundedRectangle(cornerRadius: 5))
                        .padding(.top, 12)
                }
                LazyVGrid(
                    columns: [GridItem(.flexible(), spacing: 20), GridItem(.flexible())],
                    alignment: .leading,
                    spacing: 42
                ) {
                    PrototypeModule(title: "Local API", contentHeight: 136) {
                        PrototypeLocalAPI(store: store, openSettings: { showLocalAPI = true })
                    }
                    PrototypeModule(title: "Session usage", meta: "This session", contentHeight: 136) {
                        PrototypeSessionUsage(summary: store.state.sessionUsage)
                    }
                    PrototypeModule(
                        title: "Agents",
                        action: "View all",
                        contentHeight: 320,
                        onAction: { selection = .agents }
                    ) {
                        if store.agents.isEmpty {
                            PrototypeEmpty(text: "Agent configs unavailable")
                        } else {
                            VStack(spacing: 0) {
                                ForEach(Array(store.agents.prefix(5))) { agent in
                                    PrototypeAgentRow(store: store, agent: agent, compact: true)
                                    if agent.id != store.agents.prefix(5).last?.id { Divider() }
                                }
                            }
                        }
                    }
                    PrototypeModule(
                        title: "Recent usage",
                        action: "View all",
                        contentHeight: 320,
                        onAction: { selection = .usage }
                    ) {
                        if store.state.activity.isEmpty {
                            PrototypeEmpty(text: store.isRunning ? "No requests in this session yet." : "Start protection to begin a new session.")
                        } else {
                            VStack(spacing: 0) {
                                ForEach(Array(store.state.activity.prefix(5))) { item in
                                    PrototypeUsageRow(item: item) { selectedUsage = item }
                                    if item.id != store.state.activity.prefix(5).last?.id { Divider() }
                                }
                            }
                        }
                    }
                }
                .padding(.top, 28)
            }
            .padding(.horizontal, 24)
            .padding(.top, 16)
            .padding(.bottom, 30)
            .frame(maxWidth: 960)
            .frame(maxWidth: .infinity)
        }
        .sheet(item: $selectedUsage) { ProofSheet(item: $0, identity: store.state.identity) }
        .sheet(isPresented: $showPrivacy) { PrivacySheet(state: store.state) }
        .sheet(isPresented: $showLocalAPI) { LocalApiSheet(store: store) }
    }
}

struct PrototypeAgentsPage: View {
    @ObservedObject var store: RuntimeStore

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let problem = store.state.endpointError ?? store.state.error {
                    Label(problem, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(PrototypeColor.danger)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(PrototypeColor.danger.opacity(0.09), in: RoundedRectangle(cornerRadius: 5))
                }
                Text("Enabled agents use Private AI Gateway whenever protection is on. Their previous settings return when you disconnect them.")
                    .font(.caption).foregroundStyle(.secondary)
                HStack {
                    Text("Configured agents").font(.body.weight(.semibold))
                    Spacer()
                    Text("\(store.agents.filter(\.recorded).count) enabled · \(store.agents.filter(\.connected).count) active")
                        .font(.caption).foregroundStyle(.secondary)
                }
                VStack(spacing: 0) {
                    if store.agents.isEmpty {
                        PrototypeEmpty(text: "Agent configs unavailable")
                    } else {
                        ForEach(store.agents) { agent in
                            PrototypeAgentRow(store: store, agent: agent)
                            if agent.id != store.agents.last?.id { Divider() }
                        }
                    }
                }
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
                Text("Available models sync automatically from the verified service.")
                    .font(.caption).foregroundStyle(.tertiary)
            }
            .padding(24)
            .frame(maxWidth: 920)
            .frame(maxWidth: .infinity)
        }
        .onAppear { store.reloadAgents() }
    }
}

struct PrototypeUsagePage: View {
    @ObservedObject var store: RuntimeStore
    @State private var selectedUsage: RequestActivity?
    @State private var confirmClear = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                HStack(alignment: .bottom, spacing: 12) {
                    Picker("Agent", selection: $store.usageAgent) {
                        Text("All agents").tag(String?.none)
                        ForEach(store.usage.agents, id: \.self) { Text(agentName($0)).tag(String?.some($0)) }
                    }
                    .frame(minWidth: 150)
                    Picker("Model", selection: $store.usageModel) {
                        Text("All models").tag(String?.none)
                        ForEach(store.usage.models, id: \.self) { Text($0).tag(String?.some($0)) }
                    }
                    .frame(minWidth: 190)
                    Picker("Time", selection: $store.usageRange) {
                        ForEach(UsageRange.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    Spacer()
                }
                .onChange(of: store.usageAgent) { store.reloadUsage(reset: true) }
                .onChange(of: store.usageModel) { store.reloadUsage(reset: true) }
                .onChange(of: store.usageRange) { store.reloadUsage(reset: true) }
                PrototypeUsageStats(summary: store.usage.summary)
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Usage over time").font(.body.weight(.semibold))
                        Spacer()
                        Text(store.usageRange.rawValue).font(.caption).foregroundStyle(.secondary)
                    }
                    Chart(store.usage.series) { point in
                        BarMark(x: .value("Day", point.day), y: .value("Tokens", point.tokens))
                            .foregroundStyle(PrototypeColor.accent)
                    }
                    .chartYAxis { AxisMarks(position: .leading) }
                    .frame(height: 180)
                    .padding(12)
                    .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 7))
                    .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
                }
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Usage history").font(.body.weight(.semibold))
                        Spacer()
                        Text("\(store.usage.summary.requests) records · kept on this Mac")
                            .font(.caption).foregroundStyle(.secondary)
                        Button(action: exportUsage) { Image(systemName: "square.and.arrow.down") }
                            .buttonStyle(.bordered).controlSize(.small).help("Export usage as CSV")
                        Button(role: .destructive, action: { confirmClear = true }) { Image(systemName: "trash") }
                            .buttonStyle(.bordered).controlSize(.small).help("Clear usage history")
                    }
                    VStack(spacing: 0) {
                        if store.usage.items.isEmpty {
                            PrototypeEmpty(text: "No saved usage matches these filters.")
                        } else {
                            ForEach(store.usage.items) { item in
                                PrototypeUsageRow(item: item) { selectedUsage = item }
                                if item.id != store.usage.items.last?.id { Divider() }
                            }
                        }
                    }
                    .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 7))
                    .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
                    if store.usage.nextCursor != nil {
                        Button("Load More") { store.reloadUsage(reset: false) }
                            .frame(maxWidth: .infinity)
                    }
                }
            }
            .padding(24)
            .frame(maxWidth: 920)
            .frame(maxWidth: .infinity)
        }
        .sheet(item: $selectedUsage) { ProofSheet(item: $0, identity: store.state.identity) }
        .alert("Clear all usage history?", isPresented: $confirmClear) {
            Button("Cancel", role: .cancel) {}
            Button("Clear History", role: .destructive) { store.clearUsage() }
        } message: {
            Text("This permanently deletes the local usage database. It does not affect provider records.")
        }
    }

    private func exportUsage() {
        let panel = NSSavePanel()
        panel.title = "Export Usage"
        panel.nameFieldStringValue = "private-ai-gateway-usage-\(Self.exportDateFormatter.string(from: .now)).csv"
        panel.allowedContentTypes = [.commaSeparatedText]
        guard panel.runModal() == .OK, let path = panel.url?.path else { return }
        store.exportUsage(to: path) { _ in }
    }

    private static let exportDateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .iso8601)
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()
}

private struct PrototypeUsageStats: View {
    let summary: UsageSummary

    var body: some View {
        HStack(spacing: 0) {
            PrototypeMetric(title: "Requests", value: summary.requests.formatted(), note: "\(summary.blockedLocally + summary.failedProof) failed or rejected")
            Divider()
            PrototypeMetric(title: "Tokens", value: compactNumber(summary.inputTokens + summary.outputTokens), note: "\(compactNumber(summary.inputTokens)) in · \(compactNumber(summary.outputTokens)) out")
            Divider()
            PrototypeMetric(title: "Cost", value: summary.costUsd.formatted(.currency(code: "USD")), note: "Estimated from model prices")
            Divider()
            PrototypeMetric(title: "Protected", value: protectedRate, note: "\(summary.protected) verified answers")
        }
        .frame(height: 86)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 7))
        .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
    }

    private var protectedRate: String {
        let forwarded = summary.requests - min(summary.requests, summary.blockedLocally)
        return forwarded == 0 ? "—" : "\(summary.protected * 100 / forwarded)%"
    }
}

private struct PrototypeMetric: View {
    let title: String
    let value: String
    let note: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.title3.weight(.semibold).monospacedDigit()).lineLimit(1)
            Text(note).font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
        }
        .padding(.horizontal, 13)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
    }
}

private struct PrototypeStatusSurface: View {
    @ObservedObject var store: RuntimeStore
    let openProfiles: () -> Void
    let openPrivacy: () -> Void

    private var ready: Bool {
        store.state.status == .verified && !store.state.configurationVerification && store.state.apiKeySaved
    }

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 7).fill(Color(nsColor: .controlBackgroundColor))
            if ready {
                PrototypeTrackLayer(lines: plaintextTracks, color: .primary, direction: 1)
                    .opacity(0.07)
                    .mask(LinearGradient(
                        stops: [
                            .init(color: .black, location: 0),
                            .init(color: .black, location: 0.18),
                            .init(color: .black.opacity(0.55), location: 0.34),
                            .init(color: .black.opacity(0.18), location: 0.46),
                            .init(color: .clear, location: 0.58),
                        ],
                        startPoint: .leading,
                        endPoint: .trailing
                    ))
                PrototypeTrackLayer(lines: tlsTracks, color: PrototypeColor.success, direction: -1)
                    .opacity(0.12)
                    .mask(LinearGradient(
                        stops: [
                            .init(color: .clear, location: 0.42),
                            .init(color: .black.opacity(0.18), location: 0.54),
                            .init(color: .black.opacity(0.55), location: 0.66),
                            .init(color: .black, location: 0.82),
                            .init(color: .black, location: 1),
                        ],
                        startPoint: .leading,
                        endPoint: .trailing
                    ))
            }
            RadialGradient(
                stops: [
                    .init(color: glow.opacity(ready ? 0.9 : store.isBusy ? 0.45 : 0), location: 0),
                    .init(color: glow.opacity(ready ? 0.63 : store.isBusy ? 0.3 : 0), location: 0.3),
                    .init(color: glow.opacity(ready ? 0.22 : store.isBusy ? 0.1 : 0), location: 0.65),
                    .init(color: .clear, location: 1),
                ],
                center: .center,
                startRadius: 0,
                endRadius: 210
            )
            .allowsHitTesting(false)
            HStack(spacing: 0) {
                PrototypeLocalStatus(agents: store.agents)
                PrototypeGatewayStatus(store: store, openProfiles: openProfiles)
                PrototypeRemoteStatus(store: store, openProfiles: openProfiles, openPrivacy: openPrivacy)
            }
        }
        .frame(height: 184)
        .clipShape(RoundedRectangle(cornerRadius: 7))
        .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
    }

    private var glow: Color {
        (store.isDevMode && store.isRunning ? PrototypeColor.warning : PrototypeColor.success).opacity(0.24)
    }
}

private struct PrototypeLocalStatus: View {
    let agents: [AgentStatus]

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Label("This Mac", systemImage: "laptopcomputer")
                .font(.body.weight(.semibold))
            Text("\(agents.filter(\.recorded).count) enabled · \(agents.filter(\.connected).count) active")
                .font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 6) {
                ForEach(Array(agents.filter(\.recorded).prefix(5))) { agent in
                    PrototypeAgentIcon(agent: agent, size: 24, imageSize: 15)
                }
            }
            Text("Enabled agents send their requests to the gateway on this Mac.")
                .font(.caption).foregroundStyle(.tertiary).lineLimit(2)
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

private struct PrototypeGatewayStatus: View {
    @ObservedObject var store: RuntimeStore
    let openProfiles: () -> Void

    var body: some View {
        VStack(spacing: 4) {
            NativeBrandMark(size: 44).opacity(store.isBusy ? 0.68 : 1)
            Text("Private AI Gateway").font(.headline.weight(.semibold))
            Text(statusTitle).font(.body.weight(.semibold)).foregroundStyle(statusColor)
            Toggle("Protected", isOn: Binding(
                get: { store.isRunning },
                set: { enabled in
                    if enabled && (!store.state.apiKeySaved || store.state.profiles.isEmpty) {
                        openProfiles()
                    } else {
                        store.setProtection(enabled)
                    }
                }
            ))
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.large)
            .tint(store.isDevMode ? .orange : PrototypeColor.accent)
            .disabled(store.isBusy || (store.state.endpointError != nil && !store.isRunning))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var statusTitle: String {
        store.isDevMode && store.isRunning ? "Protected · Dev mode" : store.state.status.label
    }

    private var statusColor: Color {
        if store.isDevMode && store.isRunning { return PrototypeColor.warning }
        switch store.state.status {
        case .verified: return PrototypeColor.success
        case .blocked, .error: return PrototypeColor.danger
        default: return .secondary
        }
    }
}

private struct PrototypeRemoteStatus: View {
    @ObservedObject var store: RuntimeStore
    let openProfiles: () -> Void
    let openPrivacy: () -> Void

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            HStack(spacing: 8) {
                Text(store.activeProfile?.name ?? "Custom service").font(.body.weight(.semibold))
                PrototypeProviderIcon(provider: store.activeProfile?.provider ?? .custom, size: 24)
            }
            Text(URL(string: store.state.remoteUrl ?? store.state.config.remoteUrl)?.host ?? "Not configured")
                .font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary).lineLimit(1)
            VStack(alignment: .trailing, spacing: 2) {
                Label(store.isProtected ? "Verified hardware" : "Not verified", systemImage: store.isProtected ? "checkmark" : "circle.fill")
                Label(store.isProtected ? "\(store.state.sessionUsage.protected) answers this session" : "No answers this session", systemImage: store.isProtected ? "checkmark" : "circle.fill")
            }
            .font(.caption)
            .foregroundStyle(store.isProtected ? PrototypeColor.success : .secondary)
            HStack(spacing: 6) {
                Button(action: openProfiles) { Image(systemName: "gearshape") }
                    .buttonStyle(.bordered).controlSize(.small).help("Profiles")
                Button(action: openPrivacy) { Image(systemName: "checkmark.shield") }
                    .buttonStyle(.bordered).controlSize(.small).help("Privacy verification")
                    .disabled(store.state.identity == nil)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
    }
}

private struct PrototypeTrackLayer: View {
    let lines: [String]
    let color: Color
    let direction: CGFloat

    var body: some View {
        GeometryReader { geometry in
            VStack(spacing: 0) {
                ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                    PrototypeMarquee(
                        text: line,
                        availableWidth: geometry.size.width,
                        direction: index.isMultiple(of: 2) ? direction : -direction
                    )
                    .frame(maxHeight: .infinity)
                }
            }
        }
        .foregroundStyle(color)
        .allowsHitTesting(false)
    }
}

private struct PrototypeMarquee: View {
    let text: String
    let availableWidth: CGFloat
    let direction: CGFloat

    private var distance: CGFloat { max(CGFloat(text.count) * 7.1 + 32, availableWidth) }

    var body: some View {
        TimelineView(.animation) { context in
            let period = max(Double(distance / 20), 1)
            let phase = CGFloat(context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: period)) * 20
            Text("\(text)    \(text)")
                .font(.system(size: 12, design: .monospaced))
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
                .offset(x: direction > 0 ? -distance + phase : -phase)
                .frame(width: availableWidth, alignment: .leading)
                .clipped()
        }
    }
}

private struct PrototypeModule<Content: View>: View {
    let title: String
    var meta: String?
    var action: String?
    let contentHeight: CGFloat
    var onAction: (() -> Void)?
    @ViewBuilder let content: Content

    init(
        title: String,
        meta: String? = nil,
        action: String? = nil,
        contentHeight: CGFloat,
        onAction: (() -> Void)? = nil,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.meta = meta
        self.action = action
        self.contentHeight = contentHeight
        self.onAction = onAction
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title).font(.body.weight(.semibold))
                if let meta { Text(meta).font(.caption).foregroundStyle(.secondary) }
                Spacer()
                if let action, let onAction {
                    Button(action, action: onAction).buttonStyle(.link).font(.caption)
                }
            }
            content
                .frame(maxWidth: .infinity, minHeight: contentHeight, maxHeight: contentHeight, alignment: .topLeading)
                .background(Color(nsColor: .controlBackgroundColor))
                .clipShape(RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
        }
    }
}

private struct PrototypeLocalAPI: View {
    @ObservedObject var store: RuntimeStore
    let openSettings: () -> Void
    @State private var reveal = false
    @State private var copied: String?

    var body: some View {
        VStack(spacing: 0) {
            PrototypeCopyRow(
                title: "Endpoint",
                side: store.state.proxyUrl == nil ? "Stopped" : "Available",
                value: store.state.proxyUrl ?? "Unavailable",
                copied: copied == "Endpoint",
                enabled: store.state.proxyUrl != nil,
                actionSymbol: "gearshape",
                actionHelp: "Local API settings",
                onCopy: { copy("Endpoint", store.state.proxyUrl) },
                onAction: openSettings
            )
            Divider()
            PrototypeCopyRow(
                title: "Client key",
                side: "for your own tools",
                value: store.clientKey.isEmpty ? "Unavailable" : reveal ? store.clientKey : "\(store.clientKey.prefix(4))••••••••••••",
                copied: copied == "Client key",
                enabled: !store.clientKey.isEmpty,
                actionSymbol: reveal ? "eye.slash" : "eye",
                actionHelp: reveal ? "Hide client key" : "Reveal client key",
                onCopy: { copy("Client key", store.clientKey) },
                onAction: { reveal.toggle() }
            )
        }
    }

    private func copy(_ label: String, _ value: String?) {
        guard let value, !value.isEmpty else { return }
        store.copy(value)
        copied = label
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            if copied == label { copied = nil }
        }
    }
}

private struct PrototypeCopyRow: View {
    let title: String
    let side: String
    let value: String
    let copied: Bool
    let enabled: Bool
    let actionSymbol: String
    let actionHelp: String
    let onCopy: () -> Void
    let onAction: () -> Void
    @State private var hovering = false

    var body: some View {
        ZStack(alignment: .trailing) {
            Button(action: onCopy) {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 8) {
                        Text(title).font(.body.weight(.semibold))
                        Spacer()
                        Text(side).font(.caption).foregroundStyle(.secondary)
                    }
                    Text(value)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .padding(.leading, 12)
                .padding(.trailing, 98)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!enabled)
            .onHover { hovering = $0 }
            HStack(spacing: 12) {
                Text(copied ? "Copied" : "Copy")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(copied ? PrototypeColor.success : .secondary)
                    .opacity(hovering || copied ? 1 : 0)
                Button(action: onAction) { Image(systemName: actionSymbol) }
                    .buttonStyle(.bordered).controlSize(.small).help(actionHelp)
            }
            .padding(.trailing, 12)
        }
        .frame(maxHeight: .infinity)
    }
}

private struct PrototypeSessionUsage: View {
    let summary: UsageSummary

    var body: some View {
        Grid(horizontalSpacing: 0, verticalSpacing: 0) {
            GridRow {
                PrototypeSessionMetric(title: "Requests", value: summary.requests.formatted())
                PrototypeSessionMetric(title: "Tokens", value: compactNumber(summary.inputTokens + summary.outputTokens))
            }
            GridRow {
                PrototypeSessionMetric(title: "Cost", value: summary.costUsd.formatted(.currency(code: "USD")))
                PrototypeSessionMetric(title: "Protected", value: protectedRate)
            }
        }
    }

    private var protectedRate: String {
        let forwarded = summary.requests - min(summary.requests, summary.blockedLocally)
        return forwarded == 0 ? "—" : "\(summary.protected * 100 / forwarded)%"
    }
}

private struct PrototypeSessionMetric: View {
    let title: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.title3.weight(.semibold).monospacedDigit()).lineLimit(1)
        }
        .padding(.horizontal, 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .overlay(alignment: .trailing) { Divider() }
        .overlay(alignment: .bottom) { Divider() }
    }
}

struct PrototypeAgentRow: View {
    @ObservedObject var store: RuntimeStore
    let agent: AgentStatus
    var compact = false

    var body: some View {
        HStack(spacing: 12) {
            PrototypeAgentIcon(agent: agent, size: 32, imageSize: 20)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 7) {
                    Text(agent.name).font(.body.weight(.semibold))
                    PrototypeState(text: presence.label, tone: presence.tone, symbol: presence.symbol)
                }
                if !compact {
                    Text(agent.attention ?? agent.error ?? (agent.installed ? abbreviated(agent.configPath) : "CLI not found"))
                        .font(.system(.caption, design: agent.installed ? .monospaced : .default))
                        .foregroundStyle(agent.error == nil ? .secondary : PrototypeColor.danger)
                        .lineLimit(agent.attention == nil && agent.error == nil ? 1 : 2)
                }
            }
            Spacer(minLength: 12)
            Toggle("Connected", isOn: Binding(
                get: { agent.recorded },
                set: { store.setAgent(agent, connected: $0) }
            ))
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.small)
            .tint(PrototypeColor.accent)
            .disabled(store.isBusy || (!agent.recorded && (!agent.installed || !store.isProtected || store.state.catalog == nil)))
        }
        .padding(.horizontal, 12)
        .frame(height: compact ? 63 : 68)
        .contentShape(Rectangle())
    }

    private var presence: (label: String, tone: PrototypeTone, symbol: String?) {
        if agent.error != nil { return ("Error", .danger, "exclamationmark.triangle.fill") }
        if agent.attention != nil { return ("Needs attention", .warning, "exclamationmark.triangle.fill") }
        if agent.connected { return ("Connected", .success, "checkmark.shield.fill") }
        if agent.installed { return ("Not connected", .neutral, nil) }
        return ("CLI not found", .neutral, nil)
    }
}

private struct PrototypeAgentIcon: View {
    let agent: AgentStatus
    let size: CGFloat
    let imageSize: CGFloat

    var body: some View {
        Group {
            if let image = NativeAsset.image("agents/\(agent.id).svg") {
                Image(nsImage: image).resizable().scaledToFit()
                    .frame(width: imageSize, height: imageSize)
            } else {
                Text(String(agent.name.prefix(2)).uppercased()).font(.caption2.weight(.bold))
            }
        }
        .frame(width: size, height: size)
        .background(Color.white, in: RoundedRectangle(cornerRadius: min(8, size / 4)))
        .overlay(RoundedRectangle(cornerRadius: min(8, size / 4)).stroke(Color(nsColor: .separatorColor).opacity(0.7)))
    }
}

struct PrototypeUsageRow: View {
    let item: RequestActivity
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 7) {
                        Text(agentName(item.agent)).font(.body.weight(.semibold))
                        PrototypeState(text: outcome.label, tone: outcome.tone, symbol: outcome.symbol)
                    }
                    Text(item.model ?? item.path)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 10)
                PrototypeAmount(value: tokenValue, label: "tokens")
                PrototypeAmount(value: item.costUsd?.formatted(.currency(code: "USD")) ?? "—", label: "cost")
                Text(Date(timeIntervalSince1970: TimeInterval(item.at)), format: .dateTime.month(.abbreviated).day().hour().minute())
                    .font(.caption).foregroundStyle(.secondary).frame(width: 96, alignment: .trailing)
            }
            .padding(.horizontal, 12)
            .frame(height: 63)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var tokenValue: String {
        let value = (item.inputTokens ?? 0) + (item.outputTokens ?? 0)
        return value == 0 ? "—" : compactNumber(value)
    }

    private var outcome: (label: String, tone: PrototypeTone, symbol: String?) {
        if !item.leftDevice { return ("Blocked locally", .neutral, "nosign") }
        if item.verified == true { return ("Protected", .success, "checkmark.shield.fill") }
        if item.verified == false { return ("Proof failed", .danger, "xmark.shield.fill") }
        if item.status < 200 || item.status >= 300 { return ("Upstream failed", .danger, "exclamationmark.triangle.fill") }
        return ("Proof unavailable", .warning, "questionmark.diamond")
    }
}

private struct PrototypeAmount: View {
    let value: String
    let label: String

    var body: some View {
        VStack(alignment: .trailing, spacing: 0) {
            Text(value).font(.caption.weight(.semibold).monospacedDigit())
            Text(label).font(.caption2).foregroundStyle(.tertiary)
        }
        .frame(width: 62, alignment: .trailing)
    }
}

private enum PrototypeTone { case success, warning, danger, neutral }

private struct PrototypeState: View {
    let text: String
    let tone: PrototypeTone
    let symbol: String?

    var body: some View {
        HStack(spacing: 4) {
            if let symbol { Image(systemName: symbol).font(.caption2) }
            else { Circle().frame(width: 5, height: 5) }
            Text(text)
        }
        .font(.caption2.weight(.medium))
        .foregroundStyle(color)
    }

    private var color: Color {
        switch tone {
        case .success: PrototypeColor.success
        case .warning: PrototypeColor.warning
        case .danger: PrototypeColor.danger
        case .neutral: .secondary
        }
    }
}

private struct PrototypeProviderIcon: View {
    let provider: ServiceProvider
    let size: CGFloat

    var body: some View {
        Group {
            if let image = NativeAsset.image(providerPath) {
                Image(nsImage: image).resizable().scaledToFit()
            } else {
                Image(systemName: "network")
            }
        }
        .frame(width: size, height: size)
    }

    private var providerPath: String {
        switch provider {
        case .phala: "providers/phala.svg"
        case .redpill: "providers/redpill.png"
        case .custom: "providers/custom.svg"
        }
    }
}

private struct PrototypeEmpty: View {
    let text: String

    var body: some View {
        Text(text).font(.caption).foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(20)
    }
}

private func abbreviated(_ path: String) -> String {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    return path.hasPrefix(home) ? "~\(path.dropFirst(home.count))" : path
}

private func agentName(_ id: String?) -> String {
    switch id {
    case "claude-code": "Claude Code"
    case "codex": "Codex"
    case "opencode": "OpenCode"
    case "pi": "Pi"
    case "hermes": "Hermes"
    case .some(let id): id
    case .none: "Unknown agent"
    }
}

private func compactNumber(_ value: UInt64) -> String {
    if value >= 1_000_000 { return String(format: "%.1fM", Double(value) / 1_000_000) }
    if value >= 1_000 { return String(format: "%.1fK", Double(value) / 1_000) }
    return value.formatted()
}

private let plaintextTracks = [
    "POST /v1/messages   model demo/verified-chat-01   stream true",
    "event content_block_delta   The public compose hash matches the expected value.",
    "messages user   Inspect the public dstack attestation report.",
    "event message_delta   stop_reason end_turn   output_tokens 96",
    "POST /v1/responses   model demo/verified-reasoning-01   store false",
    "event response.output_text.delta   Release notes summarized in three points.",
    "input user   Summarize the public release notes.   tool read_public_file",
    "event response.completed   input_tokens 384   output_tokens 96",
    "POST /v1/chat/completions   Compare tdx_quote digest with compose_hash.",
    "data chat.completion.chunk   Both digests match.",
    "tools function compare_hash   tool_choice auto",
]

private let tlsTracks = [
    "17 03 03 00 f4  9f3a c1e0 7b42 d5a8 0e6f 2c91 4d17 e8b3",
    "17 03 03 03 1a  4d17 e8b3 5a0c f9d2 61b7 a3e4 b8c5 0f2e",
    "application_data record_len 244  17 03 03 00 f4  6d03 c1e8 5f27 b6a9",
    "17 03 03 01 6c  e1a7 5c09 f38d 2b64 d0e7 4a1f 6b0c 8e52",
    "17 03 03 00 5e  b2a0 7e95 0d1b 9c6e 18e4 a0f7 5b3c d29a",
    "application_data record_len 794  17 03 03 03 1a  b5c8 02a6 5f7e 1b93",
    "17 03 03 02 48  9e21 4fb7 a6d5 0c83 e2f9 71b4 5d0e 8ac6",
    "17 03 03 00 91  81c7 e6a2 5b9d 1f74 c0e3 a8d6 4e27 9b1c",
    "17 03 03 01 d0  a3e8 5c02 e9b1 4d7f 8a36 1e0c b5d9 7f23",
    "application_data record_len 152  17 03 03 00 98  c7d2 3e5a 90f4 1b6c",
    "17 03 03 00 3c  7a0d 2c95 f6e3 41b8 d9c0 3f5e 8a2b 6e17",
]
