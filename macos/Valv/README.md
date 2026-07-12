# macOS App

The `Valv` Xcode project builds the native macOS client and its extensions.

- `Valv` (`Valv/`): the menu-bar app, onboarding flow, device pairing, and File Provider domain management.
- `ValvFileProvider` (`ValvFileProvider/`): the File Provider extension. See [`ValvFileProvider/README.md`](ValvFileProvider/README.md).
- `ValvFinderSync` (`ValvFinderSync/`): the Finder Sync extension. See [`ValvFinderSync/README.md`](ValvFinderSync/README.md).
- `ValvTests` / `ValvUITests`: the app's test targets.

All three shipping targets reach the local `valvd` daemon through [`../DaemonKit`](../DaemonKit) over loopback TCP. App Sandbox denies `connect()` on the daemon's Unix socket, so the sandboxed extensions discover the TCP port `valvd` advertises through the app group instead.

See [`../../README.md`](../../README.md) for install links and [`../../contracts/ipc/README.md`](../../contracts/ipc/README.md) for the control protocol these targets consume.
