import Foundation

struct RuntimeFailure: LocalizedError {
    let code: String
    let message: String
    var errorDescription: String? { message }
}

final class RuntimeClient {
    private let process = Process()
    private let input = Pipe()
    private let output = Pipe()
    private let lock = NSLock()
    private var buffer = Data()
    private var nextId: UInt64 = 1
    private var pending: [String: (Result<Any, Error>) -> Void] = [:]
    var onState: ((GatewayState) -> Void)?
    var onExit: ((Error?) -> Void)?

    func start() throws {
        let service = try serviceURL()
        guard FileManager.default.isExecutableFile(atPath: service.path) else {
            throw RuntimeFailure(
                code: "runtime_missing",
                message: "The bundled desktop runtime is missing or cannot be executed."
            )
        }
        process.executableURL = service
        process.standardInput = input
        process.standardOutput = output
        process.standardError = FileHandle.standardError
        process.terminationHandler = { [weak self] process in
            let error = process.terminationStatus == 0 ? nil : RuntimeFailure(
                code: "runtime_exited",
                message: "The desktop runtime exited with status \(process.terminationStatus)."
            )
            DispatchQueue.main.async { self?.onExit?(error) }
        }
        output.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if data.isEmpty { return }
            self?.receive(data)
        }
        try process.run()
    }

    func request<ResultType: Decodable, Params: Encodable>(
        _ method: String,
        params: Params,
        completion: @escaping (Result<ResultType, Error>) -> Void
    ) {
        lock.lock()
        let id = String(nextId)
        nextId += 1
        pending[id] = { result in
            switch result {
            case .success(let value):
                do {
                    let data = try JSONSerialization.data(withJSONObject: value)
                    completion(.success(try JSONDecoder().decode(ResultType.self, from: data)))
                } catch {
                    completion(.failure(error))
                }
            case .failure(let error): completion(.failure(error))
            }
        }
        lock.unlock()

        do {
            let paramsData = try JSONEncoder().encode(params)
            let paramsObject = try JSONSerialization.jsonObject(with: paramsData)
            let message: [String: Any] = [
                "schemaVersion": 1,
                "id": id,
                "method": method,
                "params": paramsObject,
            ]
            var data = try JSONSerialization.data(withJSONObject: message)
            data.append(0x0A)
            try input.fileHandleForWriting.write(contentsOf: data)
        } catch {
            resolve(id: id, result: .failure(error))
        }
    }

    func shutdown() {
        requestVoid("shutdown", params: EmptyParams()) { _ in }
    }

    func terminate() {
        output.fileHandleForReading.readabilityHandler = nil
        try? input.fileHandleForWriting.close()
        if process.isRunning { process.terminate() }
    }

    private func receive(_ data: Data) {
        lock.lock()
        buffer.append(data)
        var lines: [Data] = []
        while let newline = buffer.firstIndex(of: 0x0A) {
            lines.append(buffer[..<newline])
            buffer.removeSubrange(...newline)
        }
        lock.unlock()
        for line in lines where !line.isEmpty { handle(line) }
    }

    private func handle(_ data: Data) {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return
        }
        if object["event"] as? String == "stateChanged", let payload = object["payload"] {
            guard JSONSerialization.isValidJSONObject(payload),
                  let stateData = try? JSONSerialization.data(withJSONObject: payload),
                  let state = try? JSONDecoder().decode(GatewayState.self, from: stateData)
            else { return }
            DispatchQueue.main.async { [weak self] in self?.onState?(state) }
            return
        }
        guard let id = object["id"] as? String else { return }
        if let error = object["error"] as? [String: Any] {
            resolve(id: id, result: .failure(RuntimeFailure(
                code: error["code"] as? String ?? "operation_failed",
                message: error["message"] as? String ?? "The operation failed."
            )))
        } else {
            resolve(id: id, result: .success(object["result"] ?? NSNull()))
        }
    }

    private func resolve(id: String, result: Result<Any, Error>) {
        lock.lock()
        let callback = pending.removeValue(forKey: id)
        lock.unlock()
        guard let callback else { return }
        DispatchQueue.main.async { callback(result) }
    }

    private func requestVoid<Params: Encodable>(
        _ method: String,
        params: Params,
        completion: @escaping (Result<Void, Error>) -> Void
    ) {
        request(method, params: params) { (result: Result<JSONNull, Error>) in
            completion(result.map { _ in () })
        }
    }

    private func serviceURL() throws -> URL {
        if let override = ProcessInfo.processInfo.environment["PRIVATE_AI_GATEWAY_RUNTIME"],
           !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        return Bundle.main.bundleURL
            .appendingPathComponent("Contents/MacOS/private-ai-gateway-desktop-service")
    }
}

private struct JSONNull: Decodable {
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        guard container.decodeNil() else {
            throw DecodingError.typeMismatch(
                JSONNull.self,
                .init(codingPath: decoder.codingPath, debugDescription: "Expected null")
            )
        }
    }
}
