import Foundation
import XCTest
@testable import PrivateAIGatewayMac

final class RuntimeProtocolCodecTests: XCTestCase {
    func testEncodesEmptyParamsAsAnObject() throws {
        let data = try RuntimeProtocolCodec.encodeRequest(
            id: "request-1",
            method: "getState",
            params: EmptyParams()
        )
        let envelope = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )

        XCTAssertEqual(envelope["schemaVersion"] as? Int, 1)
        XCTAssertEqual(envelope["id"] as? String, "request-1")
        XCTAssertEqual(envelope["method"] as? String, "getState")
        XCTAssertNotNil(envelope["params"] as? [String: Any])
    }

    func testDecodesScalarResults() throws {
        let result = try RuntimeProtocolCodec.decodeResult(
            String.self,
            from: "pag_local_client_key"
        )

        XCTAssertEqual(result, "pag_local_client_key")
    }
}
