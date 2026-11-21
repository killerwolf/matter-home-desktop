// Dessine le logo Ostal « Seuil » (assets/logo/e-seuil.svg : une arche ouverte et un point
// allumé, quelqu'un est là) en PNG de 1024 px.
// Usage : swift scripts/make-icon.swift SORTIE.png [arrondi|plein]
//   arrondi (défaut) : carré arrondi façon macOS avec sa marge de 10 % (icône des applications,
//                      favicon) ;
//   plein            : le fond occupe tout le carré, sans marge ni coins (iOS et Android
//                      arrondissent eux-mêmes : apple-touch-icon, icônes de l'app web).
// Mêmes cotes que le SVG, dans le repère d'AppKit (y vers le haut : y = 1024 - y du SVG).

import AppKit

let size: CGFloat = 1024
let full = CommandLine.arguments.count > 2 && CommandLine.arguments[2] == "plein"
// Dessin dans un bitmap de 1024 px exactement (pas d'échelle d'écran).
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size),
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

func color(_ hex: UInt32) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: 1)
}

// Fond : dégradé vertical #10232d (haut) → #163444 (bas).
let bg = full
    ? NSBezierPath(rect: CGRect(x: 0, y: 0, width: size, height: size))
    : NSBezierPath(roundedRect: CGRect(x: 100, y: 100, width: 824, height: 824),
                   xRadius: 185, yRadius: 185)
NSGradient(starting: color(0x10232d), ending: color(0x163444))!.draw(in: bg, angle: -90)

// L'arche : deux montants (x = 332 et 692, du bas y = 760 à la naissance y = 470 du SVG) et un
// demi-cercle de rayon 180 par le haut ; trait de 64, bouts ronds.
let arch = NSBezierPath()
arch.move(to: NSPoint(x: 332, y: size - 760))
arch.line(to: NSPoint(x: 332, y: size - 470))
arch.appendArc(withCenter: NSPoint(x: 512, y: size - 470), radius: 180,
               startAngle: 180, endAngle: 0, clockwise: true)
arch.line(to: NSPoint(x: 692, y: size - 760))
arch.lineWidth = 64
arch.lineCapStyle = .round
arch.lineJoinStyle = .round
color(0xf3efe6).setStroke()
arch.stroke()

// Le point allumé, sous l'arche.
color(0xf5b33f).setFill()
NSBezierPath(ovalIn: NSRect(x: 512 - 58, y: size - 668 - 58, width: 116, height: 116)).fill()

NSGraphicsContext.restoreGraphicsState()
let png = rep.representation(using: .png, properties: [:])!
try! png.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
