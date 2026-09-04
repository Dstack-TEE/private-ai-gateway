import AppKit
import Charts
import SwiftUI

enum AppSection: String, CaseIterable, Identifiable {
    case overview = "Overview"
    case agents = "Agents"
    case usage = "Usage"
    case settings = "Settings"
    var id: String { rawValue }
    var symbol: String {
        switch self {
        case .overview: "shield.lefthalf.filled"
        case .agents: "terminal"
        case .usage: "chart.bar.xaxis"
        case .settings: "gearshape"
        }
    }
}

struct MainWindowView: View {
    @ObservedObject var store: RuntimeStore
    @State private var selection: AppSection? = .overview
    @State private var showProfiles = false
    @State private var showNewProfile = false

    var body: some View {
        NavigationSplitView {
            List(AppSection.allCases, selection: $selection) { section in
                Label(section.rawValue, systemImage: section.symbol)
                    .tag(section)
                    .padding(.vertical, 5)
            }
            .navigationSplitViewColumnWidth(min: 190, ideal: 218)
            .safeAreaInset(edge: .top) {
                AppIdentity()
                    .padding(.horizontal, 14)
                    .padding(.top, 8)
            }
        } detail: {
            VStack(spacing: 0) {
                PageHeader(
                    title: selection?.rawValue ?? "Overview",
                    store: store,
                    configure: openProfiles
                )
                Divider()
                Group {
                    switch selection ?? .overview {
                    case .overview: OverviewPage(store: store, showProfiles: $showProfiles)
                    case .agents: AgentsPage(store: store)
                    case .usage: UsagePageView(store: store)
                    case .settings: SettingsPage(store: store)
                    }
                }
            }
        }
        .sheet(isPresented: $showProfiles) {
            ProfilesSheet(store: store)
        }
        .sheet(isPresented: $showNewProfile) {
            ProfileEditor(store: store, profile: nil)
        }
        .alert("Private AI Gateway", isPresented: Binding(
            get: { store.errorMessage != nil },
            set: { if !$0 { store.errorMessage = nil } }
        )) {
            Button("OK") { store.errorMessage = nil }
        } message: {
            Text(store.errorMessage ?? "")
        }
    }

    private func openProfiles() {
        if store.state.profiles.isEmpty { showNewProfile = true }
        else { showProfiles = true }
    }
}

private struct AppIdentity: View {
    var body: some View {
        HStack(spacing: 10) {
            Image(nsImage: NSWorkspace.shared.icon(forFile: Bundle.main.bundlePath))
                .resizable()
                .frame(width: 32, height: 32)
                .shadow(color: .black.opacity(0.12), radius: 2, y: 1)
            VStack(alignment: .leading, spacing: 1) {
                Text("Private AI Gateway").font(.headline)
                Text("Confidential inference").font(.caption).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct PageHeader: View {
    let title: String
    @ObservedObject var store: RuntimeStore
    let configure: () -> Void

    var body: some View {
        HStack {
            Text(title).font(.title2.weight(.semibold))
            Spacer()
            if store.isDevMode && store.isRunning {
                Label("Dev mode", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .font(.callout.weight(.medium))
            }
            Text(store.state.status.label)
                .foregroundStyle(statusColor)
            Toggle("Protected", isOn: Binding(
                get: { store.isRunning },
                set: { enabled in
                    if enabled && (!store.state.apiKeySaved || store.state.profiles.isEmpty) {
                        configure()
                    } else {
                        store.setProtection(enabled)
                    }
                }
            ))
            .toggleStyle(.switch)
            .disabled(store.isBusy)
        }
        .padding(.horizontal, 24)
        .frame(height: 58)
    }

    private var statusColor: Color {
        if store.isDevMode && store.isRunning { return .orange }
        switch store.state.status {
        case .verified: return .green
        case .blocked, .error: return .red
        default: return .secondary
        }
    }
}

struct OverviewPage: View {
    @ObservedObject var store: RuntimeStore
    @Binding var showProfiles: Bool
    @State private var proof: RequestActivity?
    @State private var privacy = false

    var body: some View {
        ScrollView {
            VStack(spacing: 28) {
                ProtectionSummary(store: store, showProfiles: $showProfiles, showPrivacy: $privacy)
                HStack(alignment: .top, spacing: 20) {
                    LocalApiOverview(store: store)
                    AgentOverview(store: store)
                }
                UsageSummaryGrid(summary: store.state.sessionUsage, title: "This session")
                SectionGroup(title: "Recent usage", trailing: nil) {
                    if store.state.activity.isEmpty {
                        ContentUnavailableView("No usage this session", systemImage: "clock")
                            .frame(height: 110)
                    } else {
                        ForEach(store.state.activity.prefix(5)) { item in
                            UsageRow(item: item) { proof = item }
                            if item.id != store.state.activity.prefix(5).last?.id { Divider() }
                        }
                    }
                }
            }
            .padding(28)
        }
        .sheet(item: $proof) { ProofSheet(item: $0, identity: store.state.identity) }
        .sheet(isPresented: $privacy) { PrivacySheet(state: store.state) }
    }
}

private struct ProtectionSummary: View {
    @ObservedObject var store: RuntimeStore
    @Binding var showProfiles: Bool
    @Binding var showPrivacy: Bool

    var body: some View {
        HStack(spacing: 18) {
            Image(systemName: store.isProtected ? "checkmark.shield.fill" : "shield")
                .font(.system(size: 34))
                .foregroundStyle(store.isDevMode && store.isRunning ? .orange : store.isProtected ? .green : .secondary)
            VStack(alignment: .leading, spacing: 5) {
                Text(store.isDevMode && store.isRunning ? "Protected in dev mode" : store.state.status.label)
                    .font(.title3.weight(.semibold))
                Text(store.state.progress ?? store.state.error ?? store.activeProfile?.name ?? "Choose a Confidential AI profile")
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer()
            Button("Profiles…") { showProfiles = true }
            Button("Privacy Verification…") { showPrivacy = true }
                .disabled(store.state.identity == nil)
        }
        .padding(20)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
    }
}

private struct LocalApiOverview: View {
    @ObservedObject var store: RuntimeStore
    @State private var reveal = false
    var body: some View {
        SectionGroup(title: "Local API", trailing: nil) {
            CopyRow(title: "Endpoint", value: store.state.proxyUrl ?? "Unavailable") {
                if let endpoint = store.state.proxyUrl { store.copy(endpoint) }
            }
            Divider()
            HStack {
                Button {
                    store.copy(store.clientKey)
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Client key").font(.caption).foregroundStyle(.secondary)
                        Text(reveal ? store.clientKey : "pag_••••••••••••")
                            .font(.system(.body, design: .monospaced))
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.plain)
                Button { reveal.toggle() } label: {
                    Image(systemName: reveal ? "eye.slash" : "eye")
                }
                .help(reveal ? "Hide client key" : "Reveal client key")
            }
            .padding(.vertical, 8)
        }
    }
}

private struct AgentOverview: View {
    @ObservedObject var store: RuntimeStore
    var body: some View {
        SectionGroup(title: "Agents", trailing: nil) {
            ForEach(store.agents.prefix(5)) { agent in
                AgentRow(store: store, agent: agent)
                if agent.id != store.agents.prefix(5).last?.id { Divider() }
            }
        }
    }
}

struct AgentsPage: View {
    @ObservedObject var store: RuntimeStore
    var body: some View {
        List(store.agents) { agent in AgentRow(store: store, agent: agent) }
            .listStyle(.inset)
            .onAppear { store.reloadAgents() }
    }
}

private struct AgentRow: View {
    @ObservedObject var store: RuntimeStore
    let agent: AgentStatus
    var body: some View {
        HStack {
            Image(systemName: "terminal.fill").foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(agent.name)
                if let detail = agent.attention ?? agent.error {
                    Text(detail).font(.caption).foregroundStyle(agent.error == nil ? .orange : .red).lineLimit(2)
                } else {
                    Text(agent.installed ? agent.configPath : "CLI not found")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer()
            Toggle("Connected", isOn: Binding(
                get: { agent.connected },
                set: { store.setAgent(agent, connected: $0) }
            ))
            .labelsHidden()
            .toggleStyle(.switch)
        }
        .padding(.vertical, 7)
    }
}

struct UsagePageView: View {
    @ObservedObject var store: RuntimeStore
    @State private var proof: RequestActivity?
    @State private var confirmClear = false

    var body: some View {
        VStack(spacing: 18) {
            HStack {
                Picker("Agent", selection: $store.usageAgent) {
                    Text("All agents").tag(String?.none)
                    ForEach(store.usage.agents, id: \.self) { Text($0).tag(String?.some($0)) }
                }
                Picker("Model", selection: $store.usageModel) {
                    Text("All models").tag(String?.none)
                    ForEach(store.usage.models, id: \.self) { Text($0).tag(String?.some($0)) }
                }
                Picker("Time", selection: $store.usageRange) {
                    ForEach(UsageRange.allCases) { Text($0.rawValue).tag($0) }
                }
                Spacer()
                Button("Clear History…", role: .destructive) { confirmClear = true }
            }
            .onChange(of: store.usageAgent) { store.reloadUsage(reset: true) }
            .onChange(of: store.usageModel) { store.reloadUsage(reset: true) }
            .onChange(of: store.usageRange) { store.reloadUsage(reset: true) }
            UsageSummaryGrid(summary: store.usage.summary, title: nil)
            Chart(store.usage.series) { point in
                BarMark(x: .value("Day", point.day), y: .value("Tokens", point.tokens))
                    .foregroundStyle(.green)
            }
            .chartYAxis { AxisMarks(position: .leading) }
            .frame(height: 180)
            List(store.usage.items) { item in UsageRow(item: item) { proof = item } }
                .listStyle(.inset)
            if store.usage.nextCursor != nil {
                Button("Load More") { store.reloadUsage(reset: false) }
            }
        }
        .padding(24)
        .sheet(item: $proof) { ProofSheet(item: $0, identity: store.state.identity) }
        .alert("Clear all usage history?", isPresented: $confirmClear) {
            Button("Cancel", role: .cancel) {}
            Button("Clear History", role: .destructive) { store.clearUsage() }
        } message: {
            Text("This permanently deletes the local usage database. It does not affect provider records.")
        }
    }
}

struct SettingsPage: View {
    @ObservedObject var store: RuntimeStore
    @State private var profiles = false
    @State private var localApi = false
    @State private var advanced = false
    @State private var confirmRestore = false

    var body: some View {
        Form {
            Section("Confidential AI") {
                LabeledContent("Profile", value: store.activeProfile?.name ?? "Not configured")
                Button("Manage Profiles…") { profiles = true }
                    .disabled(store.isRunning)
            }
            Section("Local API") {
                Button("Local API Settings…") { localApi = true }
                    .disabled(store.isRunning)
                Button("Rotate Client Key…") { store.rotateClientKey() }
            }
            DisclosureGroup("Advanced", isExpanded: $advanced) {
                LabeledContent("OS policy", value: store.isDevMode ? "Development OS allowed" : "Production OS required")
                Button("Restore All Agent Configurations…", role: .destructive) { confirmRestore = true }
            }
        }
        .formStyle(.grouped)
        .padding(12)
        .sheet(isPresented: $profiles) { ProfilesSheet(store: store) }
        .sheet(isPresented: $localApi) { LocalApiSheet(store: store) }
        .alert("Restore all agent configurations?", isPresented: $confirmRestore) {
            Button("Cancel", role: .cancel) {}
            Button("Restore All", role: .destructive) { store.restoreAllAgents() }
        } message: {
            Text("Private AI Gateway will revoke every managed agent token and restore its previous configuration where possible.")
        }
    }
}

private struct UsageSummaryGrid: View {
    let summary: UsageSummary
    let title: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let title { Text(title).font(.headline) }
            HStack(spacing: 28) {
                Metric(title: "Requests", value: summary.requests.formatted())
                Metric(title: "Tokens", value: (summary.inputTokens + summary.outputTokens).formatted())
                Metric(title: "Cost", value: summary.costUsd.formatted(.currency(code: "USD")))
                Metric(title: "Protected", value: summary.requests == 0 ? "—" : "\(summary.protected * 100 / summary.requests)%")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct Metric: View {
    let title: String
    let value: String
    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.title3.monospacedDigit())
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct UsageRow: View {
    let item: RequestActivity
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: item.verified == true ? "checkmark.shield.fill" : item.leftDevice ? "exclamationmark.triangle.fill" : "nosign")
                    .foregroundStyle(item.verified == true ? .green : item.leftDevice ? .red : .secondary)
                VStack(alignment: .leading, spacing: 3) {
                    Text(item.model ?? item.path).lineLimit(1)
                    Text([item.agent, item.path].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer()
                Text(((item.inputTokens ?? 0) + (item.outputTokens ?? 0)).formatted())
                    .monospacedDigit().foregroundStyle(.secondary)
                Text(Date(timeIntervalSince1970: TimeInterval(item.at)), style: .time)
                    .foregroundStyle(.secondary).frame(width: 62, alignment: .trailing)
                Image(systemName: "chevron.right").foregroundStyle(.tertiary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.vertical, 7)
    }
}

private struct CopyRow: View {
    let title: String
    let value: String
    let copy: () -> Void
    var body: some View {
        Button(action: copy) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.caption).foregroundStyle(.secondary)
                    Text(value).font(.system(.body, design: .monospaced)).lineLimit(1)
                }
                Spacer()
                Image(systemName: "doc.on.doc")
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.vertical, 8)
    }
}

private struct SectionGroup<Content: View>: View {
    let title: String
    let trailing: AnyView?
    @ViewBuilder let content: Content
    init(title: String, trailing: AnyView?, @ViewBuilder content: () -> Content) {
        self.title = title
        self.trailing = trailing
        self.content = content()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack { Text(title).font(.headline); Spacer(); trailing }
            VStack(spacing: 0) { content }
                .padding(.horizontal, 14)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(.separator.opacity(0.5)))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
