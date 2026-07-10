import SwiftUI

/// Header content each onboarding page supplies to `OnboardingCardChrome` - the image,
/// title, and description are common across all six pages; everything else (the
/// interactive controls below them) stays each page's own view code.
struct OnboardingPageMetadata {
    let imageName: String
    let heroSymbolName: String?
    let heroImageName: String?
    let title: String
    let description: String

    init(
        imageName: String,
        heroSymbolName: String? = nil,
        heroImageName: String? = nil,
        title: String,
        description: String
    ) {
        self.imageName = imageName
        self.heroSymbolName = heroSymbolName
        self.heroImageName = heroImageName
        self.title = title
        self.description = description
    }
}

/// TourKit-style card chrome (github.com/rampatra/TourKit), built fresh rather than
/// vendored - TourKit's own `TourPage` model is a fixed image+title+description+button
/// template with no room for the live interactive controls several of our pages need
/// (daemon-reconciliation state, sign-in waiting state, a folder-picker/text-field
/// cluster). This ports its presentational half (image region with bottom gradient
/// fade, page-indicator dots, back/close icon-button overlay, bottom panel text
/// treatment) and replaces its fixed button with a generic `@ViewBuilder` content slot.
struct OnboardingCardChrome<Content: View>: View {
    let metadata: OnboardingPageMetadata
    let pageIndex: Int
    let totalPages: Int
    /// Hard-requirement pages (daemon setup, sign-in) SHALL NOT present a way to
    /// bail out early (macos-app spec) - hiding the close button is how this chrome
    /// enforces that, the same way the back chevron is hidden on page 0.
    let canClose: Bool
    let onBack: () -> Void
    let onClose: () -> Void
    @ViewBuilder let content: () -> Content

    static var cardWidth: CGFloat { 440 }
    static var imageAspectRatio: CGFloat { 16.0 / 10.0 }
    var imageHeight: CGFloat { (Self.cardWidth / Self.imageAspectRatio).rounded() }

    static var brandTeal: Color { Color("AccentColor") }
    static var brandTealDeep: Color { Color(red: 0x19 / 255, green: 0x34 / 255, blue: 0x29 / 255) }

    var body: some View {
        VStack(spacing: 0) {
            imageSection
            bottomPanel
        }
        .frame(width: Self.cardWidth)
        .background(Color(white: 0.10))
        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .stroke(Color.white.opacity(0.10), lineWidth: 1)
        }
        // Cross-fades title/description/content as one unit when the page changes,
        // matching TourKit's own per-slide `.id(currentIndex)` + `.transition(.opacity)`.
        .id(pageIndex)
        .transition(.opacity)
        .animation(.easeInOut(duration: 0.25), value: pageIndex)
    }

    private var imageSection: some View {
        ZStack(alignment: .top) {
            Image(metadata.imageName)
                .resizable()
                .scaledToFill()
                .frame(width: Self.cardWidth, height: imageHeight)
                .clipped()

            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0),
                    .init(color: Color(white: 0.10).opacity(0.15), location: 0.25),
                    .init(color: Color(white: 0.10).opacity(0.45), location: 0.50),
                    .init(color: Color(white: 0.10).opacity(0.80), location: 0.75),
                    .init(color: Color(white: 0.10), location: 1.0)
                ],
                startPoint: .top,
                endPoint: .bottom
            )
            .frame(height: 180)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
            .allowsHitTesting(false)

            PageIndicator(totalPages: totalPages, currentIndex: pageIndex)
                .padding(.bottom, 14)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                .allowsHitTesting(false)

            hero

            topControls
        }
        .frame(width: Self.cardWidth, height: imageHeight)
    }

    @ViewBuilder
    private var hero: some View {
        if let heroImageName = metadata.heroImageName {
            Image(heroImageName)
                .resizable()
                .scaledToFit()
                .frame(width: 120, height: 120)
                .shadow(color: .black.opacity(0.22), radius: 10, y: 5)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
                .allowsHitTesting(false)
        } else if let heroSymbolName = metadata.heroSymbolName {
            Image(systemName: heroSymbolName)
                .symbolRenderingMode(.hierarchical)
                .font(.system(size: 72, weight: .semibold))
                .foregroundStyle(.white.opacity(0.88))
                .shadow(color: .black.opacity(0.22), radius: 10, y: 5)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
                .allowsHitTesting(false)
        }
    }

    private var topControls: some View {
        HStack {
            iconButton(systemName: "chevron.left", label: "Back", action: onBack)
                .opacity(pageIndex > 0 ? 1 : 0)
                .disabled(pageIndex == 0)

            Spacer()

            iconButton(systemName: "xmark", label: "Close", action: onClose)
                .opacity(canClose ? 1 : 0)
                .disabled(!canClose)
        }
        .padding(.horizontal, 14)
        .padding(.top, 12)
    }

    private func iconButton(systemName: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.white.opacity(0.88))
                .frame(width: 32, height: 32)
                .background {
                    Circle().fill(Color.white.opacity(0.15))
                }
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    private var bottomPanel: some View {
        VStack(spacing: 0) {
            Text(metadata.title)
                .font(.system(size: 24, weight: .bold))
                .multilineTextAlignment(.center)
                .foregroundStyle(.white)
                .fixedSize(horizontal: false, vertical: true)

            Text(metadata.description)
                .font(.body)
                .multilineTextAlignment(.center)
                .foregroundStyle(Color.white.opacity(0.70))
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 6)

            Spacer(minLength: 16)

            content()

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 32)
        .padding(.top, 12)
        .padding(.bottom, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Brand-teal capsule CTA, matching TourKit's primary-button treatment but in the
/// real product color (`OnboardingCardChrome.brandTeal`) instead of TourKit's blue.
struct OnboardingPrimaryButton: View {
    let title: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 220, height: 42)
                .background(
                    Capsule(style: .continuous)
                        .fill(
                            LinearGradient(
                                colors: [OnboardingCardChrome<EmptyView>.brandTeal, OnboardingCardChrome<EmptyView>.brandTealDeep],
                                startPoint: .top,
                                endPoint: .bottom
                            )
                        )
                )
                .clipShape(Capsule(style: .continuous))
                .contentShape(Capsule(style: .continuous))
        }
        .buttonStyle(.plain)
        .keyboardShortcut(.defaultAction)
    }
}

struct PageIndicator: View {
    let totalPages: Int
    let currentIndex: Int

    var body: some View {
        HStack(spacing: 7) {
            ForEach(0..<totalPages, id: \.self) { index in
                Capsule(style: .continuous)
                    .fill(index == currentIndex ? Color.white.opacity(0.95) : Color.white.opacity(0.32))
                    .frame(width: index == currentIndex ? 24 : 8, height: 8)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Page \(currentIndex + 1) of \(max(totalPages, 1))")
    }
}
