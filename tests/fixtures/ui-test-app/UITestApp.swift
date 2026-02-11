import SwiftUI

@main
struct UITestApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

struct ContentView: View {
    @State private var sliderValue: Double = 0.5
    @State private var textValue: String = "test"
    @State private var toggleValue: Bool = true
    @State private var selectedItem: String? = nil

    var body: some View {
        VStack(spacing: 16) {
            // Toolbar area
            HStack {
                Button("Action") {
                    print("ACTION_PRESSED")
                }
                .accessibilityIdentifier("action_button")

                Toggle("Enable", isOn: $toggleValue)
                    .accessibilityIdentifier("enable_toggle")
            }
            .padding()

            Divider()

            // Main panel
            VStack(alignment: .leading, spacing: 12) {
                Text("Volume")
                    .accessibilityIdentifier("volume_label")

                Slider(value: $sliderValue, in: 0...1)
                    .accessibilityIdentifier("volume_slider")
                    .accessibilityValue("\(sliderValue)")

                TextField("Name", text: $textValue)
                    .accessibilityIdentifier("name_field")
                    .textFieldStyle(.roundedBorder)

                List {
                    Text("Alpha").tag("Alpha")
                    Text("Beta").tag("Beta")
                    Text("Gamma").tag("Gamma")
                }
                .accessibilityIdentifier("items_list")
                .frame(height: 120)
            }
            .padding()

            // Custom canvas (no accessibility)
            Canvas { context, size in
                context.fill(
                    Path(ellipseIn: CGRect(x: 20, y: 10, width: 60, height: 60)),
                    with: .color(.blue)
                )
                context.fill(
                    Path(CGRect(x: 100, y: 10, width: 80, height: 40)),
                    with: .color(.red)
                )
            }
            .frame(height: 80)
            .accessibilityHidden(true)  // Deliberately hidden from AX

            Spacer()
        }
        .frame(width: 400, height: 500)
    }
}
