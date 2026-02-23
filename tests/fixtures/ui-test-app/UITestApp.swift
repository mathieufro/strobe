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
    @State private var dragReceived: Bool = false
    @State private var dragLabel: String = "Drag Me"

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

            Divider()

            // Drag targets for testing
            HStack(spacing: 20) {
                Text(dragLabel)
                    .frame(width: 100, height: 40)
                    .background(Color.blue.opacity(0.3))
                    .accessibilityIdentifier("drag_source")
                    .accessibilityLabel("Drag Source")
                    .draggable("drag_payload")

                Text("Drop Here")
                    .frame(width: 100, height: 40)
                    .background(dragReceived ? Color.green.opacity(0.3) : Color.gray.opacity(0.3))
                    .accessibilityIdentifier("drop_target")
                    .accessibilityLabel(dragReceived ? "Drop Received" : "Drop Here")
                    .dropDestination(for: String.self) { items, _ in
                        if let _ = items.first {
                            dragReceived = true
                            return true
                        }
                        return false
                    }
            }
            .padding()

            Spacer()
        }
        .frame(width: 400, height: 600)
    }
}
