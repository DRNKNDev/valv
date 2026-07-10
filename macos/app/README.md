# macOS App

This directory is a navigation pointer, not the app source. The implemented native macOS app lives in [`../Valv`](../Valv), an Xcode project with these targets:

- `Valv`: the menu-bar app, onboarding flow, device pairing, and File Provider domain management (`Valv/Valv`).
- `ValvFileProvider`: the File Provider extension (`Valv/ValvFileProvider`).
- `ValvFileProviderUI`: the File Provider UI extension (`Valv/ValvFileProviderUI`).
- `ValvTests` / `ValvUITests`: the app's test targets.

See [`../../README.md`](../../README.md) for install links and [`../file-provider/README.md`](../file-provider/README.md) for the File Provider extension.
