# Finder Sync Extension

`ValvFinderSync` is the macOS Finder Sync extension. It adds a single Finder context-menu action, "Share with Valv...", to items inside mounted folders.

- `FinderSync.swift`: the `FIFinderSync` subclass. It polls `GET /mounts` through the shared [`DaemonKit`](../../DaemonKit) client and sets `FIFinderSyncController.default().directoryURLs` to every mount's real local path, so the watched set follows folders as they are added and removed. When the daemon is unreachable it keeps the last-known set rather than clearing it.

Choosing "Share with Valv..." resolves the clicked path to a node id and hands off to the app by opening a `valv://share?path=...&node=...` URL. The node id is omitted if the daemon cannot resolve it, and the app falls back to the path.

The extension is sandboxed, so it reaches the local `valvd` daemon over loopback TCP rather than the Unix socket. It builds as part of the `Valv` Xcode project and is embedded in `Valv.app`. See [`../README.md`](../README.md) for the host app and the other targets, and [`../../../contracts/ipc/README.md`](../../../contracts/ipc/README.md) for the control protocol it consumes.
