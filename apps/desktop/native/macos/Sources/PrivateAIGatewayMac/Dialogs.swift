import SwiftUI

struct ProfilesSheet: View {
    @ObservedObject var store: RuntimeStore
    @Environment(\.dismiss) private var dismiss
    @State private var selection: ConfidentialProfile?
    @State private var editing: ConfidentialProfile?
    @State private var create = false
    @State private var confirmDelete = false

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Profiles").font(.title2.weight(.semibold))
                Spacer()
            }
            .padding(22)
            Divider()
            if store.state.profiles.isEmpty {
                ContentUnavailableView("No profiles", systemImage: "person.crop.circle.badge.plus", description: Text("Add a verified Confidential AI service."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(store.state.profiles, selection: $selection) { profile in
                    ProfileRow(
                        profile: profile,
                        selected: profile.id == store.state.activeProfileId
                    )
                    .tag(profile)
                    .contentShape(Rectangle())
                    .onTapGesture(count: 2) { editing = profile }
                }
                .listStyle(.inset)
            }
            Divider()
            HStack {
                Button { create = true } label: { Label("New", systemImage: "plus") }
                Button { editing = selection } label: { Label("Edit", systemImage: "pencil") }
                    .disabled(selection == nil)
                Button(role: .destructive) { confirmDelete = true } label: {
                    Label("Delete", systemImage: "trash")
                }
                .disabled(selection == nil || store.state.profiles.count < 2)
                Spacer()
                if let selection, selection.id != store.state.activeProfileId {
                    Button("Use Profile") { store.activate(selection) }
                        .disabled(selection.verifiedAt == nil)
                }
                Button("Done") { dismiss() }
            }
            .padding(16)
        }
        .frame(width: 620, height: 500)
        .onAppear { selection = store.activeProfile ?? store.state.profiles.first }
        .sheet(isPresented: $create) { ProfileEditor(store: store, profile: nil) }
        .sheet(item: $editing) { ProfileEditor(store: store, profile: $0) }
        .alert("Delete profile?", isPresented: $confirmDelete) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) {
                if let selection { store.delete(selection) }
            }
        } message: {
            Text("The profile credential will be removed from Keychain.")
        }
    }
}

private struct ProfileRow: View {
    let profile: ConfidentialProfile
    let selected: Bool
    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: providerSymbol)
                .frame(width: 26, height: 26)
                .foregroundStyle(.green)
            VStack(alignment: .leading, spacing: 3) {
                HStack {
                    Text(profile.name)
                    if profile.verifiedAt != nil {
                        Label("Verified", systemImage: "checkmark.seal.fill")
                            .font(.caption).foregroundStyle(.green)
                    }
                }
                Text(profile.remoteUrl).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            if selected { Image(systemName: "checkmark").foregroundStyle(.green) }
        }
        .padding(.vertical, 8)
    }
    private var providerSymbol: String {
        switch profile.provider {
        case .phala: "p.hexagon.fill"
        case .redpill: "r.circle.fill"
        case .custom: "server.rack"
        }
    }
}

struct ProfileEditor: View {
    @ObservedObject var store: RuntimeStore
    let profile: ConfidentialProfile?
    @Environment(\.dismiss) private var dismiss
    @State private var name: String
    @State private var provider: ServiceProvider
    @State private var endpoint: String
    @State private var key = ""
    @State private var allowDevOs: Bool

    init(store: RuntimeStore, profile: ConfidentialProfile?) {
        self.store = store
        self.profile = profile
        let provider = profile?.provider ?? .redpill
        _name = State(initialValue: profile?.name ?? provider.title)
        _provider = State(initialValue: provider)
        _endpoint = State(initialValue: profile?.remoteUrl ?? "https://tee.redpill.ai")
        _allowDevOs = State(initialValue: !store.state.config.requireProductionOs)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(profile == nil ? "New Profile" : "Edit Profile")
                    .font(.title2.weight(.semibold))
                Spacer()
                if profile?.verifiedAt != nil && key.isEmpty {
                    Label("Verified configuration", systemImage: "checkmark.seal.fill")
                        .foregroundStyle(.green)
                }
            }
            .padding(22)
            Divider()
            Form {
                TextField("Name", text: $name)
                Picker("Provider", selection: $provider) {
                    ForEach(ServiceProvider.allCases) { Text($0.title).tag($0) }
                }
                .pickerStyle(.segmented)
                TextField("Endpoint", text: $endpoint)
                    .disabled(provider != .custom)
                HStack(alignment: .firstTextBaseline) {
                    SecureField(profile?.verifiedAt == nil ? "API key" : "API key (leave blank to keep)", text: $key)
                    Button(store.isBusy ? "Verifying…" : "Verify and Save") { verify() }
                        .disabled(store.isBusy || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || endpoint.isEmpty || (profile == nil && key.isEmpty))
                }
                Toggle("Allow development OS", isOn: $allowDevOs)
                Text("Development OS mode weakens the production attestation policy and is shown in yellow whenever protection is running.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .formStyle(.grouped)
            .padding(12)
            Divider()
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
            }
            .padding(16)
        }
        .frame(width: 620, height: 460)
        .onChange(of: provider) {
            switch provider {
            case .phala: endpoint = "https://inference.phala.com"; if profile == nil { name = "Phala" }
            case .redpill: endpoint = "https://tee.redpill.ai"; if profile == nil { name = "RedPill" }
            case .custom: if profile == nil { endpoint = ""; name = "Custom" }
            }
        }
    }

    private func verify() {
        let id = profile?.id ?? "profile-\(UUID().uuidString.lowercased())"
        let input = ConfidentialProfileInput(
            id: id,
            name: name,
            provider: provider,
            remoteUrl: endpoint
        )
        store.verifyAndSave(profile: input, allowDevOs: allowDevOs, key: key) { saved in
            if saved { dismiss() }
        }
    }
}

struct LocalApiSheet: View {
    @ObservedObject var store: RuntimeStore
    @Environment(\.dismiss) private var dismiss
    @State private var config: LocalApiConfig

    init(store: RuntimeStore) {
        self.store = store
        _config = State(initialValue: store.state.localApi)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack { Text("Local API Settings").font(.title2.weight(.semibold)); Spacer() }
                .padding(22)
            Divider()
            Form {
                TextField("Listen address", text: $config.listenAddress)
                Toggle("Allow network access", isOn: $config.allowNetworkAccess)
                TextField("Port", value: $config.port, format: .number)
                TextField("Client host", text: Binding(
                    get: { config.clientHost ?? "" },
                    set: { config.clientHost = $0.isEmpty ? nil : $0 }
                ))
                Text("Network access exposes the Local API beyond this Mac. Connected agents must be disconnected before changing the endpoint.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .formStyle(.grouped)
            .padding(12)
            Divider()
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                Button("Save") { store.saveLocalApi(config) { if $0 { dismiss() } } }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(16)
        }
        .frame(width: 580, height: 430)
    }
}

struct ProofSheet: View {
    let item: RequestActivity
    let identity: GatewayIdentity?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label(verdict, systemImage: item.verified == true ? "checkmark.shield.fill" : item.leftDevice ? "exclamationmark.triangle.fill" : "nosign")
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(item.verified == true ? .green : item.leftDevice ? .red : .secondary)
                Spacer()
            }
            .padding(22)
            Divider()
            ScrollView {
                Grid(alignment: .leading, horizontalSpacing: 22, verticalSpacing: 12) {
                    proofRow("Request", item.id)
                    proofRow("Agent", item.agent ?? "Unknown")
                    proofRow("Model", item.model ?? "Not reported")
                    proofRow("Path", "\(item.method) \(item.path)")
                    proofRow("Status", String(item.status))
                    proofRow("Receipt", item.receiptId ?? "No receipt")
                    proofRow("Policy", item.locallyConstrained == true ? "Applied before forwarding" : "Not reported")
                    proofRow("Rewrite", item.rewritten == true ? "Service rewrote the request" : "No rewrite reported")
                    proofRow("Delivery", item.leftDevice ? "Request may have left this Mac" : "Blocked locally before delivery")
                    proofRow("Input tokens", item.inputTokens?.formatted() ?? "Not reported")
                    proofRow("Output tokens", item.outputTokens?.formatted() ?? "Not reported")
                    proofRow("Cost", item.costUsd?.formatted(.currency(code: "USD")) ?? "Not reported")
                    proofRow("Gateway keyset", identity?.keysetDigest ?? "Not available")
                    proofRow("Detail", item.detail.isEmpty ? "No additional detail" : item.detail)
                }
                .textSelection(.enabled)
                .padding(24)
            }
            Divider()
            HStack { Spacer(); Button("Done") { dismiss() } }.padding(16)
        }
        .frame(width: 680, height: 620)
    }

    private var verdict: String {
        if item.verified == true { return "Proof verified" }
        if !item.leftDevice { return "Blocked locally" }
        if item.verified == false { return "Proof failed" }
        return "Proof unavailable"
    }

    @ViewBuilder private func proofRow(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(.secondary).gridColumnAlignment(.trailing)
            Text(value).gridColumnAlignment(.leading)
        }
    }
}

struct PrivacySheet: View {
    let state: GatewayState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Privacy Verification").font(.title2.weight(.semibold))
                Spacer()
                Label(state.status == .verified ? "Verified" : state.status.label, systemImage: "checkmark.seal.fill")
                    .foregroundStyle(state.status == .verified ? .green : .secondary)
            }
            .padding(22)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    Text("The gateway verifies the workload identity and model catalog before it forwards requests. Each response receipt binds the request, verified upstream session, and returned response.")
                    if let identity = state.identity {
                        privacyGroup("Workload identity", rows: [
                            ("TEE", identity.teeType),
                            ("Trust level", identity.trustLevel),
                            ("Keyset digest", identity.keysetDigest),
                            ("Serving mode", identity.serving),
                            ("TLS SPKI", identity.tlsSpki ?? "Not published"),
                        ])
                        privacyGroup("Source provenance", rows: [
                            ("Repository", identity.source.repoUrl ?? "Not published"),
                            ("Commit", identity.source.repoCommit ?? "Not published"),
                            ("Image digest", identity.source.imageDigest ?? "Not published"),
                        ])
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Verification checks").font(.headline)
                        ForEach(state.checks) { check in
                            HStack(alignment: .top) {
                                Image(systemName: check.status == "pass" ? "checkmark.circle.fill" : check.status == "fail" ? "xmark.circle.fill" : "info.circle.fill")
                                    .foregroundStyle(check.status == "pass" ? .green : check.status == "fail" ? .red : .secondary)
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(check.title)
                                    Text(check.detail).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
                .textSelection(.enabled)
                .padding(24)
            }
            Divider()
            HStack { Spacer(); Button("Done") { dismiss() } }.padding(16)
        }
        .frame(width: 720, height: 650)
    }

    private func privacyGroup(_ title: String, rows: [(String, String)]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title).font(.headline)
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                LabeledContent(row.0, value: row.1)
            }
        }
    }
}
