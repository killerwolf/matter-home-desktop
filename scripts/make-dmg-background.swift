// Dessine le fond de la fenêtre du DMG (600 × 400 points) : une flèche de l'emplacement
// de l'application vers celui du dossier Applications, et la consigne au-dessus.
// Les positions doivent correspondre à `icon_locations` de scripts/dmg-settings.py.
//
// Usage : swift scripts/make-dmg-background.swift <sortie.png> <échelle 1|2>

import AppKit

let args = CommandLine.arguments
let output = args[1]
let scale = CGFloat(Double(args[2]) ?? 1)
let width: CGFloat = 600, height: CGFloat = 400

let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(width * scale), pixelsHigh: Int(height * scale),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: width, height: height)  // points : le Finder affiche l'image à 600 × 400

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

// Fond clair, léger dégradé vers le bleu de l'icône.
NSGradient(colors: [NSColor(calibratedWhite: 0.99, alpha: 1),
                    NSColor(calibratedRed: 0.90, green: 0.94, blue: 0.98, alpha: 1)])!
    .draw(in: NSRect(x: 0, y: 0, width: width, height: height), angle: -90)

// Consigne (repère AppKit : origine en bas à gauche ; le Finder compte depuis le haut).
let title = "Glissez Matter Home Desktop dans Applications" as NSString
let style = NSMutableParagraphStyle()
style.alignment = .center
let attrs: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 17, weight: .semibold),
    .foregroundColor: NSColor(calibratedRed: 0.20, green: 0.27, blue: 0.36, alpha: 1),
    .paragraphStyle: style,
]
title.draw(in: NSRect(x: 0, y: height - 72, width: width, height: 28), withAttributes: attrs)

// Flèche entre les deux icônes (centres à x = 150 et 450, y = 190 depuis le haut).
let y = height - 190
let blue = NSColor(calibratedRed: 0.30, green: 0.60, blue: 0.88, alpha: 1)
blue.setStroke()
blue.setFill()
let shaft = NSBezierPath()
shaft.move(to: NSPoint(x: 240, y: y))
shaft.line(to: NSPoint(x: 342, y: y))
shaft.lineWidth = 10
shaft.lineCapStyle = .round
shaft.stroke()
let head = NSBezierPath()
head.move(to: NSPoint(x: 366, y: y))
head.line(to: NSPoint(x: 336, y: y + 22))
head.line(to: NSPoint(x: 336, y: y - 22))
head.close()
head.lineJoinStyle = .round
head.lineWidth = 6
head.fill()
head.stroke()

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: output))
