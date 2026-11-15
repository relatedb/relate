import Foundation

// MARK: - Putting a store where an app keeps it

extension Database {
    /// Where an app keeps a store it manages: `name` inside the user's
    /// Application Support directory — on macOS, inside a folder named for
    /// the app's bundle identifier there, since that directory is shared
    /// between apps. (A process with no bundle identifier, such as a
    /// command-line tool, gets `name` directly in that directory.) The
    /// containing directory is created if needed; the store itself is not.
    ///
    /// Application Support (not Documents) is the place for data the user
    /// never browses, and it is backed up.
    ///
    /// `name` is one path component: an empty name, `.`, `..` or a name
    /// with a `/` in it is refused with ``RelateError/invalidParameter(_:)``
    /// rather than resolved to somewhere outside that directory.
    public static func defaultStoreURL(named name: String) throws -> URL {
        let base = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
        #if os(macOS)
        let bundleIdentifier = Bundle.main.bundleIdentifier
        #else
        let bundleIdentifier: String? = nil
        #endif
        return try storeURL(named: name, in: base, bundleIdentifier: bundleIdentifier)
    }

    /// `defaultStoreURL` over an explicit base directory.
    static func storeURL(named name: String, in base: URL, bundleIdentifier: String?) throws -> URL {
        guard !name.isEmpty, name != ".", name != "..", !name.contains("/") else {
            throw RelateError.invalidParameter("a store name is one path component, got \(quotedWire(name))")
        }
        var directory = base
        if let bundleIdentifier { directory.appendPathComponent(bundleIdentifier, isDirectory: true) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.appendingPathComponent(name)
    }

    /// Copies a seed store to `target` when nothing is there yet: the
    /// first-launch step for an app that ships a store in its bundle. Returns
    /// `true` when it copied, `false` when `target` already existed (in
    /// which case nothing is touched).
    ///
    /// A bundled store cannot be opened where it lies: an app bundle is
    /// read-only and a store writes beside itself. The copy is made under a
    /// temporary name in `target`'s directory and renamed into place, so an
    /// interrupted launch never leaves half a store. A temporary copy an
    /// interrupted launch left behind is removed by a later call, once it
    /// was started more than ten minutes ago (a younger one may be another
    /// launch's copy in progress). The copy is made writable by its owner
    /// only (bundle resources are read-only), and on iOS is given
    /// `protection` — by default `completeUntilFirstUserAuthentication`,
    /// which keeps the store usable while the device is locked after its
    /// first unlock.
    ///
    /// Works for both store forms: a single `.rl` file and a store
    /// directory. A seed that is a symbolic link is followed. Both URLs
    /// follow the rule of ``init(url:readOnly:options:)``: a URL that is
    /// not a local file URL is ``RelateError/invalidParameter(_:)``. Throws
    /// the file system's own error if the seed is missing or the copy
    /// fails.
    @discardableResult
    public static func provision(
        at target: URL, from seed: URL,
        protection: FileProtectionType? = .completeUntilFirstUserAuthentication
    ) throws -> Bool {
        try requireLocalFile(target)
        try requireLocalFile(seed)
        let files = FileManager.default
        let copy = ProvisioningCopy(target: target)
        // Before the early return, so a leftover is cleaned up on a later
        // launch even though the store is in place by then.
        copy.removeStale()
        if files.fileExists(atPath: target.path) { return false }
        try files.createDirectory(at: copy.directory, withIntermediateDirectories: true)
        let temporary = copy.freshURL()
        do {
            // A link is copied as a link; the store has to be the real thing.
            try files.copyItem(at: seed.resolvingSymlinksInPath(), to: temporary)
            try makeOwnerWritable(temporary, protection: protection)
            try files.moveItem(at: temporary, to: target)
        } catch {
            try? files.removeItem(at: temporary)
            // Another launch of the same app won the race to the same name:
            // the store is there, which is all the caller asked for.
            if files.fileExists(atPath: target.path) { return false }
            throw error
        }
        return true
    }

    /// The copy and, for a store directory, everything in it, each with
    /// whether it is a directory.
    private static func items(of root: URL) throws -> [(url: URL, isDirectory: Bool)] {
        let rootIsDirectory = try isDirectory(root)
        var items = [(url: root, isDirectory: rootIsDirectory)]
        guard rootIsDirectory else { return items }
        for path in try FileManager.default.subpathsOfDirectory(atPath: root.path) {
            let url = root.appendingPathComponent(path)
            items.append((url, try isDirectory(url)))
        }
        return items
    }

    /// Whether the item at `url` is a directory. An item that cannot be
    /// found is an error: it is never taken for a file, since the answer
    /// decides the permissions the item is given.
    static func isDirectory(_ url: URL) throws -> Bool {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory) else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSFilePathErrorKey: url.path])
        }
        return isDirectory.boolValue
    }

    /// Owner-only, writable permissions on the copy and everything in it
    /// (a directory needs its search bit), plus the protection class.
    private static func makeOwnerWritable(_ root: URL, protection: FileProtectionType?) throws {
        for item in try items(of: root) {
            try FileManager.default.setAttributes(
                [.posixPermissions: item.isDirectory ? 0o700 : 0o600], ofItemAtPath: item.url.path)
            try applyProtection(protection, to: item.url)
        }
    }

    /// Sets a file's protection class. It is applied on iOS only; elsewhere
    /// there is nothing to set.
    private static func applyProtection(_ protection: FileProtectionType?, to url: URL) throws {
        #if os(iOS)
        if let protection {
            try FileManager.default.setAttributes([.protectionKey: protection], ofItemAtPath: url.path)
        }
        #endif
    }
}

/// The temporary copy a provision stages beside its target: how it is
/// named, and how the leftovers of an interrupted provision are found.
///
/// A copy is named `.<target>.<started>-<uuid>.provisioning`: hidden,
/// specific to its target, and carrying the second it was started. The
/// start time is in the name because the file's own dates are no guide: a
/// copy keeps its seed's modification date.
struct ProvisioningCopy {
    let target: URL

    private static let suffix = ".provisioning"

    /// How long after its start a leftover counts as abandoned. A younger
    /// one may be the copy another launch is making right now (an app and
    /// its extension starting together), and removing it would fail that
    /// launch.
    static let staleAfter: TimeInterval = 600

    private var prefix: String { ".\(target.lastPathComponent)." }

    var directory: URL { target.deletingLastPathComponent() }

    /// A new name for one attempt started at `start`.
    func freshURL(startedAt start: Date = Date()) -> URL {
        let seconds = Int(start.timeIntervalSince1970)
        return directory.appendingPathComponent("\(prefix)\(seconds)-\(UUID().uuidString)\(Self.suffix)")
    }

    /// When the copy named `name` was started, if `name` is one of this
    /// target's copies. The whole name has to parse: `.library.` is also
    /// how the copies of a target named `library.rl` begin, and those are
    /// not this target's.
    private func start(ofCopyNamed name: String) -> Date? {
        guard name.hasPrefix(prefix), name.hasSuffix(Self.suffix) else { return nil }
        let middle = name.dropFirst(prefix.count).dropLast(Self.suffix.count)
        let parts = middle.split(separator: "-", maxSplits: 1)
        guard parts.count == 2, let seconds = Int(parts[0]), UUID(uuidString: String(parts[1])) != nil else {
            return nil
        }
        return Date(timeIntervalSince1970: TimeInterval(seconds))
    }

    /// Removes this target's copies that were started more than
    /// ``staleAfter`` before `now`: what an interrupted provision left.
    /// Best effort: a leftover that cannot be listed or removed only costs
    /// disk space, and must not fail the provision.
    func removeStale(asOf now: Date = Date()) {
        let files = FileManager.default
        guard let names = try? files.contentsOfDirectory(atPath: directory.path) else { return }
        for name in names {
            guard let started = start(ofCopyNamed: name), now.timeIntervalSince(started) > Self.staleAfter else {
                continue
            }
            try? files.removeItem(at: directory.appendingPathComponent(name))
        }
    }
}
