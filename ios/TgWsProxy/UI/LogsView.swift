import SwiftUI

struct LogsView: View {
    @State private var text = ""
    @State private var exportURL: URL?

    var body: some View {
        NavigationStack {
            ScrollView([.vertical, .horizontal]) {
                Text(text)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
            }
            .navigationTitle("Logs")
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button {
                        LogStore.clear()
                        reload()
                    } label: {
                        Label("Clear", systemImage: "trash")
                    }
                }
                ToolbarItemGroup(placement: .navigationBarTrailing) {
                    Button {
                        reload()
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                    if let exportURL {
                        ShareLink(item: exportURL) {
                            Label("Share", systemImage: "square.and.arrow.up")
                        }
                    }
                }
            }
            .onAppear(perform: reload)
        }
    }

    private func reload() {
        text = LogStore.read()
        exportURL = LogStore.exportURL()
    }
}
