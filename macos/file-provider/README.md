# File Provider Extension

This directory is a navigation pointer, not the extension source. The implemented File Provider extension lives in [`../Valv/ValvFileProvider`](../Valv/ValvFileProvider), with its companion UI extension in [`../Valv/ValvFileProviderUI`](../Valv/ValvFileProviderUI). Both build as part of the `Valv` Xcode project in [`../Valv`](../Valv).

- `ValvFileProvider` enumerates, materializes, and mutates items against the local `valvd` daemon over loopback TCP.
- `ValvFileProviderUI` supplies the extension's action UI (e.g. conflict/error prompts).

See [`../app/README.md`](../app/README.md) for the host app and [`../../contracts/ipc/README.md`](../../contracts/ipc/README.md) for the `fileprovider` control protocol these targets consume.
