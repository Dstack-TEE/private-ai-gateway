import Foundation

enum GatewayStatus: String, Codable {
    case stopped, verifying, verified, blocked, error
}

enum ServiceProvider: String, Codable, CaseIterable, Identifiable {
    case phala, redpill, custom
    var id: String { rawValue }
    var title: String {
        switch self {
        case .phala: "Phala"
        case .redpill: "RedPill"
        case .custom: "Custom"
        }
    }
}

struct ProfileAuth: Codable {
    let kind: String
    let accountId: String?
    let accountName: String?
}

struct ConfidentialProfile: Codable, Identifiable, Hashable {
    let id: String
    var name: String
    var provider: ServiceProvider
    var remoteUrl: String
    let auth: ProfileAuth
    let verifiedAt: UInt64?

    static func == (lhs: Self, rhs: Self) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

struct ConfidentialProfileInput: Codable {
    let id: String
    let name: String
    let provider: ServiceProvider
    let remoteUrl: String
}

struct StartGatewayConfig: Codable {
    let remoteUrl: String
    let requireProductionOs: Bool
}

struct LocalApiConfig: Codable {
    var listenAddress: String
    var allowNetworkAccess: Bool
    var port: UInt16
    var clientHost: String?
}

struct VerificationCheck: Codable, Identifiable {
    let id: String
    let section: String
    let title: String
    let status: String
    let detail: String
}

struct SourceProvenance: Codable {
    let repoUrl: String?
    let repoCommit: String?
    let imageDigest: String?
}

struct GatewayIdentity: Codable {
    let teeType: String
    let trustLevel: String
    let keysetDigest: String
    let keysetNotAfter: UInt64
    let tlsSpki: String?
    let source: SourceProvenance
    let serving: String
    let supportedE2eeVersions: [String]
}

struct RequestActivity: Codable, Identifiable, Hashable {
    let id: String
    let sessionId: String
    let method: String
    let path: String
    let model: String?
    let status: UInt16
    let streamed: Bool
    let receiptId: String?
    let verified: Bool?
    let detail: String
    let at: UInt64
    let agent: String?
    let locallyConstrained: Bool?
    let rewritten: Bool?
    let leftDevice: Bool
    let inputTokens: UInt64?
    let outputTokens: UInt64?
    let cacheReadTokens: UInt64?
    let cacheWriteTokens: UInt64?
    let costUsd: Double?
}

struct UsageSummary: Codable {
    let requests: UInt64
    let inputTokens: UInt64
    let outputTokens: UInt64
    let cacheReadTokens: UInt64
    let cacheWriteTokens: UInt64
    let costUsd: Double
    let protected: UInt64
    let blockedLocally: UInt64
    let failedProof: UInt64

    static let zero = Self(
        requests: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0,
        cacheWriteTokens: 0, costUsd: 0, protected: 0, blockedLocally: 0,
        failedProof: 0
    )
}

struct UsagePoint: Codable, Identifiable {
    let day: String
    let requests: UInt64
    let inputTokens: UInt64
    let outputTokens: UInt64
    let tokens: UInt64
    let costUsd: Double
    var id: String { day }
}

struct UsagePage: Codable {
    let items: [RequestActivity]
    let nextCursor: String?
    let summary: UsageSummary
    let series: [UsagePoint]
    let agents: [String]
    let models: [String]
}

struct ModelSummary: Codable, Identifiable {
    let id: String
    let name: String
    let contextLength: UInt64?
    let maxOutputLength: UInt64?
    let isTee: Bool?
    let inputPricePerMillion: Double?
    let outputPricePerMillion: Double?
    let cacheReadPricePerMillion: Double?
    let cacheWritePricePerMillion: Double?
    let inputModalities: [String]
    let outputModalities: [String]
    let capabilities: [String]
    let description: String?
}

struct CatalogSummary: Codable {
    let revision: String
    let fetchedAt: UInt64
    let models: [ModelSummary]
    let removed: [String]
}

struct UsageQuery: Codable {
    var agent: String?
    var model: String?
    var sessionId: String?
    var since: UInt64?
    var until: UInt64?
    var cursor: String?
    var limit: Int?
}

struct AgentStatus: Codable, Identifiable {
    let id: String
    let name: String
    let configPath: String
    let installed: Bool
    let connected: Bool
    let recorded: Bool
    let authorized: Bool
    let attention: String?
    let error: String?
}

struct ConfigChange: Codable, Identifiable {
    let key: String
    let before: String?
    let after: String?
    let sensitive: Bool
    var id: String { key }
}

struct ConnectOptions: Codable { let defaultModel: String? }

struct AgentPreview: Codable {
    let agent: AgentStatus
    let connect: Bool
    let changes: [ConfigChange]
    let note: String
    let revision: String
}

struct GatewayState: Codable {
    let status: GatewayStatus
    let configurationVerification: Bool
    let progress: String?
    let remoteUrl: String?
    let proxyUrl: String?
    let endpointError: String?
    let identity: GatewayIdentity?
    let checks: [VerificationCheck]
    let activity: [RequestActivity]
    let sessionId: String?
    let sessionUsage: UsageSummary
    let usageRevision: UInt64
    let error: String?
    let config: StartGatewayConfig
    let profiles: [ConfidentialProfile]
    let activeProfileId: String
    let localApi: LocalApiConfig
    let apiKeySaved: Bool
    let catalog: CatalogSummary?

    static let loading = Self(
        status: .stopped, configurationVerification: false, progress: nil,
        remoteUrl: nil, proxyUrl: nil, endpointError: nil, identity: nil,
        checks: [], activity: [], sessionId: nil, sessionUsage: .zero,
        usageRevision: 0, error: nil,
        config: .init(remoteUrl: "", requireProductionOs: true), profiles: [],
        activeProfileId: "", localApi: .init(
            listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180,
            clientHost: nil
        ), apiKeySaved: false, catalog: nil
    )
}

struct EmptyParams: Codable {}
struct StartParams: Codable { let config: StartGatewayConfig }
struct VerifyParams: Codable {
    let profile: ConfidentialProfileInput
    let requireProductionOs: Bool
    let key: String?
}
struct ProfileParams: Codable { let profileId: String }
struct UsageParams: Codable { let query: UsageQuery }
struct ExportUsageParams: Codable { let query: UsageQuery; let path: String }
struct AgentParams: Codable {
    let agentId: String
    let connect: Bool
    let options: ConnectOptions
}
struct ApplyAgentParams: Codable {
    let agentId: String
    let connect: Bool
    let revision: String
    let options: ConnectOptions
}
struct LocalApiParams: Codable { let config: LocalApiConfig }
