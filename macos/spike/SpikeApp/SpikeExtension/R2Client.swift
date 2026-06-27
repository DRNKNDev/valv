import Foundation

struct R2ObjectRecord: Codable, Hashable {
    let key: String
    let size: Int64?
    let contentType: String?
}

final class R2Client {
    enum RequestKind {
        case download
        case upload
        case delete
    }

    enum Error: LocalizedError {
        case invalidBaseURL(String)
        case invalidResponse(URL)
        case httpStatus(Int, URL)
        case invalidHelperResponse(URL)

        var errorDescription: String? {
            switch self {
            case .invalidBaseURL(let value):
                return "Invalid presignBaseURL: \(value)"
            case .invalidResponse(let url):
                return "Invalid HTTP response for \(url.absoluteString)"
            case .httpStatus(let status, let url):
                return "Unexpected HTTP \(status) for \(url.absoluteString)"
            case .invalidHelperResponse(let url):
                return "Invalid helper response from \(url.absoluteString)"
            }
        }
    }

    let session: URLSession

    private static let appGroupIdentifier = "group.dev.drnkn.SpikeApp"
    private static let objectListCacheFileName = "object-list-cache.json"

    private let config: SpikeConfig
    private let decoder = JSONDecoder()
    private let cache = MetadataCache()

    init(config: SpikeConfig, session: URLSession? = nil) {
        self.config = config
        self.session = session ?? URLSession(configuration: .default)
    }

    func presignedGetURL(for key: String) async throws -> URL {
        try await presignedURL(for: key, kind: .download)
    }

    func upload(fileAt localURL: URL, to key: String) async throws {
        var request = URLRequest(url: try await presignedURL(for: key, kind: .upload))
        request.httpMethod = "PUT"

        let (_, response) = try await session.upload(for: request, fromFile: localURL)
        try validate(response: response, url: request.url, allowing404: false)

        await cache.upsertObject(R2ObjectRecord(key: key, size: nil, contentType: nil))
        try await persistCacheIfAvailable()
    }

    func delete(key: String) async throws {
        var request = URLRequest(url: try await presignedURL(for: key, kind: .delete))
        request.httpMethod = "DELETE"

        let (_, response) = try await session.data(for: request)
        try validate(response: response, url: request.url, allowing404: true)

        await cache.removeObject(forKey: key)
        try await persistCacheIfAvailable()
    }

    func listObjects() async throws -> [R2ObjectRecord] {
        if let inMemory = await cache.allObjects(), !inMemory.isEmpty {
            TraceLogger.log("list-objects cache-hit memory count=\(inMemory.count)")
            return inMemory
        }

        if let diskObjects = try loadObjectsFromDisk(), !diskObjects.isEmpty {
            TraceLogger.log("list-objects cache-hit disk count=\(diskObjects.count)")
            await cache.update(diskObjects)
            return diskObjects
        }

        let helperURL = try endpoint(path: "/objects")
        NSLog("R2Client listing objects via %@", helperURL.absoluteString)
        TraceLogger.log("list-objects start \(helperURL.absoluteString)")
        let (data, response) = try await session.data(from: helperURL)
        try validate(response: response, url: helperURL, allowing404: false)

        let objects = try decoder.decode([R2ObjectRecord].self, from: data).sorted { $0.key < $1.key }
        NSLog("R2Client received %ld objects", objects.count)
        TraceLogger.log("list-objects success count=\(objects.count)")
        await cache.update(objects)
        try saveObjectsToDisk(objects)
        return objects
    }

    func item(forKey key: String) async throws -> R2ObjectRecord? {
        if let cached = await cache.object(forKey: key) {
            return cached
        }

        return try await listObjects().first(where: { $0.key == key })
    }

    func cancelAll() {
        session.invalidateAndCancel()
    }

    func validate(response: URLResponse, url: URL?, allowing404: Bool) throws {
        guard let responseURL = url else {
            throw Error.invalidBaseURL(config.presignBaseURL)
        }

        guard let httpResponse = response as? HTTPURLResponse else {
            throw Error.invalidResponse(responseURL)
        }

        let statusCode = httpResponse.statusCode
        if (200 ... 299).contains(statusCode) || (allowing404 && statusCode == 404) {
            return
        }

        throw Error.httpStatus(statusCode, responseURL)
    }

    private func presignedURL(for key: String, kind: RequestKind) async throws -> URL {
        let path: String

        switch kind {
        case .download:
            path = "/presign/download"
        case .upload:
            path = "/presign/upload"
        case .delete:
            path = "/presign/delete"
        }

        let helperURL = try endpoint(path: path, queryItems: [URLQueryItem(name: "key", value: key)])
        NSLog("R2Client requesting %@ presign for %@ via %@", String(describing: kind), key, helperURL.absoluteString)
        TraceLogger.log("presign \(String(describing: kind)) key=\(key) start")
        let (data, response) = try await session.data(from: helperURL)
        try validate(response: response, url: helperURL, allowing404: false)

        let helperResponse = try decoder.decode(PresignResponse.self, from: data)

        guard let url = URL(string: helperResponse.url) else {
            throw Error.invalidHelperResponse(helperURL)
        }

        NSLog("R2Client received presigned URL for %@", key)
        TraceLogger.log("presign \(String(describing: kind)) key=\(key) success")

        return url
    }

    private func endpoint(path: String, queryItems: [URLQueryItem] = []) throws -> URL {
        guard var components = URLComponents(string: config.presignBaseURL) else {
            throw Error.invalidBaseURL(config.presignBaseURL)
        }

        let basePath = components.path.hasSuffix("/") ? String(components.path.dropLast()) : components.path
        components.path = "\(basePath)\(path)"
        components.queryItems = queryItems.isEmpty ? nil : queryItems

        guard let url = components.url else {
            throw Error.invalidBaseURL(config.presignBaseURL)
        }

        return url
    }

    private func loadObjectsFromDisk() throws -> [R2ObjectRecord]? {
        guard let cacheURL = Self.objectListCacheURL(), FileManager.default.fileExists(atPath: cacheURL.path) else {
            return nil
        }

        let data = try Data(contentsOf: cacheURL)
        return try decoder.decode([R2ObjectRecord].self, from: data).sorted { $0.key < $1.key }
    }

    private func saveObjectsToDisk(_ objects: [R2ObjectRecord]) throws {
        guard let cacheURL = Self.objectListCacheURL() else {
            return
        }

        let data = try JSONEncoder().encode(objects)
        try data.write(to: cacheURL, options: Data.WritingOptions.atomic)
    }

    private func persistCacheIfAvailable() async throws {
        guard let objects = await cache.allObjects() else {
            return
        }

        try saveObjectsToDisk(objects)
    }

    private static func objectListCacheURL() -> URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier)?
            .appendingPathComponent(objectListCacheFileName)
    }
}

private struct PresignResponse: Decodable {
    let url: String
}

private actor MetadataCache {
    private var objectsByKey: [String: R2ObjectRecord] = [:]

    func update(_ objects: [R2ObjectRecord]) {
        objectsByKey = Dictionary(uniqueKeysWithValues: objects.map { ($0.key, $0) })
    }

    func upsertObject(_ object: R2ObjectRecord) {
        objectsByKey[object.key] = object
    }

    func removeObject(forKey key: String) {
        objectsByKey.removeValue(forKey: key)
    }

    func allObjects() -> [R2ObjectRecord]? {
        guard !objectsByKey.isEmpty else {
            return nil
        }

        return objectsByKey.values.sorted { $0.key < $1.key }
    }

    func object(forKey key: String) -> R2ObjectRecord? {
        objectsByKey[key]
    }
}
