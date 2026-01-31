//
//  WindowConfig.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/29/26.
//

import SwiftUI

struct WindowAccessor: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            if let window = view.window {
                window.titleVisibility = .hidden
                window.titlebarAppearsTransparent = true
                window.styleMask.insert(.fullSizeContentView)
                window.standardWindowButton(.closeButton)?.isHidden = true
                window.standardWindowButton(.miniaturizeButton)?.isHidden = true
                window.standardWindowButton(.zoomButton)?.isHidden = true
                window.isOpaque = false
                window.backgroundColor = .clear
            }
        }
        return view
    }
    func updateNSView(_ nsView: NSView, context: Context) {}
}
