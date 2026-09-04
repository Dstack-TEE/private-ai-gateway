import AppKit
import Foundation
import SwiftUI

@MainActor
final class RuntimeStore: ObservableObject {
    static let shared = RuntimeStore()
    @Published private(set) var state = GatewayState.loading
    @Published private(set) var agents: [AgentStatus] = []
    @Published private(set) var usage = UsagePage(
        items: [], nextCursor: nil, summary: .zero, series: [], agents: [], models: []
    )
    @Published private(set) var clientKey = ""
    @Published var errorMessage: String?
    @Published var isBusy = false
    @Published var usageAgent: String?
    @Published var usageModel: String?
    @Published var usageRange: UsageRange = .thirtyDays

    private let client = RuntimeClient()
    private var lastUsageRevision: UInt64 = 0

    init() {
        client.onState = { [weak self] state in self?.accept(state) }
        client.onExit = { [weak self] error in
            self?.errorMessage = error?.localizedDescription ?? "The desktop runtime stopped."
        }
        do {
            try client.start()
            reload()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func shutdown() { client.shutdown() }

    var activeProfile: ConfidentialProfile? {
        state.profiles.first { $0.id == state.activeProfileId }
    }

    var isProtected: Bool {
        state.status == .verified && !state.configurationVerification
    }

    var isRunning: Bool {
        [.verifying, .verified, .blocked].contains(state.status)
    }

    var isDevMode: Bool { !state.config.requireProductionOs }

    func reload() {
        request("getState", params: EmptyParams()) { (result: Result<GatewayState, Error>) in
            if case .success(let state) = result { self.accept(state) }
        }
        reloadAgents()
        reloadUsage(reset: true)
        request("getClientKey", params: EmptyParams()) { (result: Result<String, Error>) in
            if case .success(let key) = result { self.clientKey = key }
        }
    }

    func setProtection(_ enabled: Bool) {
        guard !isBusy else { return }
        isBusy = true
        if enabled {
            request("start", params: StartParams(config: state.config)) {
                (result: Result<GatewayState, Error>) in
                self.finishState(result)
            }
        } else {
            request("stop", params: EmptyParams()) { (result: Result<GatewayState, Error>) in
                self.finishState(result)
            }
        }
    }

    func verifyAndSave(
        profile: ConfidentialProfileInput,
        allowDevOs: Bool,
        key: String?,
        completion: @escaping (Bool) -> Void
    ) {
        guard !isBusy else { return }
        isBusy = true
        request(
            "verifyConfiguration",
            params: VerifyParams(
                profile: profile,
                requireProductionOs: !allowDevOs,
                key: key?.isEmpty == false ? key : nil
            )
        ) { (result: Result<GatewayState, Error>) in
            self.isBusy = false
            switch result {
            case .success(let state):
                self.accept(state)
                completion(true)
            case .failure(let error):
                self.errorMessage = error.localizedDescription
                completion(false)
            }
        }
    }

    func activate(_ profile: ConfidentialProfile) {
        request("activateProfile", params: ProfileParams(profileId: profile.id)) {
            (result: Result<GatewayState, Error>) in self.finishState(result)
        }
    }

    func delete(_ profile: ConfidentialProfile) {
        request("deleteProfile", params: ProfileParams(profileId: profile.id)) {
            (result: Result<GatewayState, Error>) in self.finishState(result)
        }
    }

    func clearActiveCredential() {
        request("clearApiKey", params: EmptyParams()) {
            (result: Result<GatewayState, Error>) in self.finishState(result)
        }
    }

    func reloadAgents() {
        request("listAgents", params: EmptyParams()) { (result: Result<[AgentStatus], Error>) in
            switch result {
            case .success(let agents): self.agents = agents
            case .failure(let error): self.errorMessage = error.localizedDescription
            }
        }
    }

    func setAgent(_ agent: AgentStatus, connected: Bool) {
        let defaultModel = agent.id == "codex" ? state.catalog?.models.first?.id : nil
        let options = ConnectOptions(defaultModel: defaultModel)
        request(
            "previewAgent",
            params: AgentParams(agentId: agent.id, connect: connected, options: options)
        ) { (preview: Result<AgentPreview, Error>) in
            switch preview {
            case .failure(let error): self.errorMessage = error.localizedDescription
            case .success(let preview):
                self.request(
                    "applyAgent",
                    params: ApplyAgentParams(
                        agentId: agent.id,
                        connect: connected,
                        revision: preview.revision,
                        options: options
                    )
                ) { (result: Result<AgentStatus, Error>) in
                    switch result {
                    case .success: self.reloadAgents()
                    case .failure(let error): self.errorMessage = error.localizedDescription
                    }
                }
            }
        }
    }

    func restoreAllAgents() {
        request("disconnectAllAgents", params: EmptyParams()) {
            (result: Result<[AgentStatus], Error>) in
            switch result {
            case .success(let agents): self.agents = agents
            case .failure(let error): self.errorMessage = error.localizedDescription
            }
        }
    }

    func reloadUsage(reset: Bool) {
        let now = UInt64(Date().timeIntervalSince1970)
        let query = UsageQuery(
            agent: usageAgent,
            model: usageModel,
            sessionId: nil,
            since: usageRange.since(now: now),
            until: nil,
            cursor: reset ? nil : usage.nextCursor,
            limit: 20
        )
        request("queryUsage", params: UsageParams(query: query)) {
            (result: Result<UsagePage, Error>) in
            switch result {
            case .success(let page):
                if reset {
                    self.usage = page
                } else {
                    self.usage = UsagePage(
                        items: self.usage.items + page.items,
                        nextCursor: page.nextCursor,
                        summary: page.summary,
                        series: page.series,
                        agents: page.agents,
                        models: page.models
                    )
                }
            case .failure(let error): self.errorMessage = error.localizedDescription
            }
        }
    }

    func saveLocalApi(_ config: LocalApiConfig, completion: @escaping (Bool) -> Void) {
        request("saveLocalApiConfig", params: LocalApiParams(config: config)) {
            (result: Result<GatewayState, Error>) in
            switch result {
            case .success(let state): self.accept(state); completion(true)
            case .failure(let error): self.errorMessage = error.localizedDescription; completion(false)
            }
        }
    }

    func rotateClientKey() {
        request("rotateClientKey", params: EmptyParams()) { (result: Result<String, Error>) in
            switch result {
            case .success(let key): self.clientKey = key
            case .failure(let error): self.errorMessage = error.localizedDescription
            }
        }
    }

    func copy(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    func clearUsage() {
        request("clearUsage", params: EmptyParams()) { (result: Result<UInt64, Error>) in
            switch result {
            case .success: self.reloadUsage(reset: true)
            case .failure(let error): self.errorMessage = error.localizedDescription
            }
        }
    }

    func exportUsage(to path: String, completion: @escaping (Bool) -> Void) {
        let now = UInt64(Date().timeIntervalSince1970)
        let query = UsageQuery(
            agent: usageAgent,
            model: usageModel,
            sessionId: nil,
            since: usageRange.since(now: now),
            until: nil,
            cursor: nil,
            limit: nil
        )
        request("exportUsageCsv", params: ExportUsageParams(query: query, path: path)) {
            (result: Result<Int, Error>) in
            switch result {
            case .success: completion(true)
            case .failure(let error):
                self.errorMessage = error.localizedDescription
                completion(false)
            }
        }
    }

    private func accept(_ state: GatewayState) {
        self.state = state
        if state.usageRevision != lastUsageRevision {
            lastUsageRevision = state.usageRevision
            reloadUsage(reset: true)
        }
    }

    private func finishState(_ result: Result<GatewayState, Error>) {
        isBusy = false
        switch result {
        case .success(let state): accept(state); reloadAgents()
        case .failure(let error): errorMessage = error.localizedDescription
        }
    }

    private func request<ResultType: Decodable, Params: Encodable>(
        _ method: String,
        params: Params,
        completion: @escaping (Result<ResultType, Error>) -> Void
    ) {
        client.request(method, params: params, completion: completion)
    }
}

enum UsageRange: String, CaseIterable, Identifiable {
    case sevenDays = "7 days"
    case thirtyDays = "30 days"
    case all = "All time"
    var id: String { rawValue }
    func since(now: UInt64) -> UInt64? {
        switch self {
        case .sevenDays: now - 7 * 86_400
        case .thirtyDays: now - 30 * 86_400
        case .all: nil
        }
    }
}
