import FileProvider

/// Distinguishes the two kinds of items this extension ever hands the system: a
/// synthetic per-mount folder under the root (there is one `NSFileProviderDomain` for
/// the whole account, not one per mount - see design.md D11 - so the root container's
/// children are synthesized from `GET /mounts` rather than being real nodes), and a
/// real node inside one mount's tree.
///
/// Every extension callback below (item lookup, enumeration, capabilities, rename/
/// reparent/trash/delete/create rejection) switches on this enum at its boundary
/// rather than string-prefix-checking `NSFileProviderItemIdentifier.rawValue` ad hoc in
/// each method (see design.md's synthetic-identifier-confusion risk).
public enum ValvItemIdentifier: Hashable, Sendable {
    case mount(folderId: String)
    case node(nodeId: String)

    private static let mountPrefix = "mount:"

    // The root container (`.rootContainer`) is never constructed as a `ValvItemIdentifier`
    // at all - every callback below special-cases `identifier == .rootContainer` first,
    // since it's neither a synthetic mount nor a real node.
    public init(_ identifier: NSFileProviderItemIdentifier) {
        let raw = identifier.rawValue
        if raw.hasPrefix(Self.mountPrefix) {
            self = .mount(folderId: String(raw.dropFirst(Self.mountPrefix.count)))
        } else {
            self = .node(nodeId: raw)
        }
    }

    public var rawValue: String {
        switch self {
        case .mount(let folderId):
            return "\(Self.mountPrefix)\(folderId)"
        case .node(let nodeId):
            return nodeId
        }
    }

    public var fileProviderIdentifier: NSFileProviderItemIdentifier {
        NSFileProviderItemIdentifier(rawValue)
    }

    public var isSyntheticMount: Bool {
        if case .mount = self { return true }
        return false
    }
}
