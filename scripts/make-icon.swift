// Dessine l'icône de l'application (assets/icon.png, 1024 px puis assets/icon.icns).
// Usage : scripts/make-icon.sh

import AppKit

let size: CGFloat = 1024
let img = NSImage(size: NSSize(width: size, height: size))
img.lockFocus()
let ctx = NSGraphicsContext.current!.cgContext

// Fond : carré arrondi façon macOS (marge de 10 %), dégradé bleu → sarcelle.
let inset: CGFloat = 100
let rect = CGRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
let bg = NSBezierPath(roundedRect: rect, xRadius: 185, yRadius: 185)
NSGradient(colors: [NSColor(calibratedRed: 0.13, green: 0.42, blue: 0.86, alpha: 1),
                    NSColor(calibratedRed: 0.10, green: 0.70, blue: 0.70, alpha: 1)])!
    .draw(in: bg, angle: -60)

// Maison blanche.
let white = NSColor.white
white.setFill()
let house = NSBezierPath()
house.move(to: NSPoint(x: 512, y: 760))   // faîte
house.line(to: NSPoint(x: 250, y: 530))
house.line(to: NSPoint(x: 320, y: 530))
house.line(to: NSPoint(x: 320, y: 270))
house.line(to: NSPoint(x: 704, y: 270))
house.line(to: NSPoint(x: 704, y: 530))
house.line(to: NSPoint(x: 774, y: 530))
house.close()
house.fill()

// Ondes de connexion dans la maison (couleur du fond).
let accent = NSColor(calibratedRed: 0.12, green: 0.52, blue: 0.80, alpha: 1)
accent.setStroke()
let center = NSPoint(x: 512, y: 330)
for (i, r) in [CGFloat(70), 135, 200].enumerated() {
    let arc = NSBezierPath()
    arc.appendArc(withCenter: center, radius: r, startAngle: 45, endAngle: 135)
    arc.lineWidth = 34
    arc.lineCapStyle = .round
    if i < 3 { arc.stroke() }
}
accent.setFill()
NSBezierPath(ovalIn: NSRect(x: center.x - 26, y: center.y - 26, width: 52, height: 52)).fill()

img.unlockFocus()
let tiff = img.tiffRepresentation!
let png = NSBitmapImageRep(data: tiff)!.representation(using: .png, properties: [:])!
try! png.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
