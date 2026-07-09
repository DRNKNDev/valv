import Foundation

enum SparkleUpdateError {
    static let signatureErrorDomain = "SUSparkleErrorDomain"
    static let signatureErrorCode = 3001

    static func isSignatureVerificationFailure(_ error: Error) -> Bool {
        let nsError = error as NSError
        return nsError.domain == signatureErrorDomain && nsError.code == signatureErrorCode
    }
}
