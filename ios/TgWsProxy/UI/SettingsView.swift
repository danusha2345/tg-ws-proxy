import SwiftUI

/// Every field is a draft: nothing reaches the stored settings, and therefore
/// the next proxy start, until Save is pressed. This matches the Android client,
/// where changed parameters also apply on the next start.
struct SettingsView: View {
    @EnvironmentObject private var controller: ProxyController
    @State private var draft = ProxySettings()
    @State private var portText = ""
    @State private var poolSizeText = ""
    @State private var secretDraft = ""
    @State private var showSecret = false
    @State private var notice: String?

    var body: some View {
        NavigationStack {
            Form {
                if !controller.canEditSettings {
                    Section {
                        Label(
                            "Stop the proxy to change the settings.",
                            systemImage: "lock.fill"
                        )
                        .font(.footnote)
                    }
                }

                Section("Listener") {
                    LabeledContent("Address") {
                        Text(verbatim: "127.0.0.1")
                            .foregroundStyle(.secondary)
                    }
                    HStack {
                        Text("Port")
                        Spacer()
                        TextField("1443", text: $portText)
                            .keyboardType(.numberPad)
                            .multilineTextAlignment(.trailing)
                            .frame(width: 100)
                    }
                }

                Section("Secret") {
                    if showSecret {
                        TextField("32 hex characters", text: $secretDraft)
                            .font(.body.monospaced())
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                    } else {
                        Text(verbatim: String(repeating: "•", count: 32))
                            .font(.body.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    Toggle("Show secret", isOn: $showSecret)
                    Button("Generate a new secret") {
                        if let error = controller.regenerateSecret() {
                            notice = error
                        } else {
                            secretDraft = controller.secret ?? ""
                            notice = String(localized: "A new secret was generated.")
                        }
                    }
                    Button("Copy tg://proxy link") {
                        controller.copyTelegramLink()
                        notice = String(localized: "The link was copied.")
                    }
                }

                Section("Routes") {
                    Toggle("Cloudflare fallback", isOn: $draft.fallbackCfproxy)
                    HStack {
                        Text("Connection pool")
                        Spacer()
                        TextField("4", text: $poolSizeText)
                            .keyboardType(.numberPad)
                            .multilineTextAlignment(.trailing)
                            .frame(width: 100)
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Worker domains")
                        TextField(
                            "worker.example.workers.dev",
                            text: $draft.workerDomains,
                            axis: .vertical
                        )
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .lineLimit(1...4)
                        Text("Separate several domains with a comma, a space or a new line.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Section("Masking") {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Fake TLS domain")
                        TextField("example.com", text: $draft.fakeTlsDomain)
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                            .keyboardType(.URL)
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Masking upstream")
                        TextField("upstream.example.com", text: $draft.maskingUpstream)
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                            .keyboardType(.URL)
                        Text("Requires a Fake TLS domain and must differ from it.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if let notice {
                    Section {
                        Text(notice).font(.footnote).foregroundStyle(.secondary)
                    }
                }

                Section {
                    Button("Save") { save() }
                        .disabled(!controller.canEditSettings)
                }
            }
            .disabled(!controller.canEditSettings)
            .navigationTitle("Settings")
            .onAppear(perform: loadDrafts)
        }
    }

    private func loadDrafts() {
        draft = controller.settings
        portText = String(draft.port)
        poolSizeText = String(draft.poolSize)
        secretDraft = controller.secret ?? ""
    }

    private func save() {
        guard let port = Int(portText), ProxyInputValidator.validPort(port) else {
            notice = String(localized: "The port must be between 1 and 65535.")
            return
        }
        guard let poolSize = Int(poolSizeText), ProxyInputValidator.validPoolSize(poolSize) else {
            notice = String(localized: "The pool size must be between 0 and 128.")
            return
        }
        let secret = secretDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        if secret != controller.secret, let error = controller.replaceSecret(with: secret) {
            notice = error
            return
        }

        draft.port = port
        draft.poolSize = poolSize
        controller.settings = draft
        controller.saveSettings()
        loadDrafts()
        notice = String(localized: "Saved. The values apply on the next start.")
    }
}
