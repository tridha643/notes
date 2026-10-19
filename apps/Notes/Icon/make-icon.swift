// Draws the Notes app icon at 1024 px and writes AppIcon.icns next to this file.
// Run: swift apps/Notes/Icon/make-icon.swift
import AppKit

let size: CGFloat = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size),
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext

func color(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: a)
}
func rounded(_ r: CGRect, _ radius: CGFloat) -> CGPath {
    CGPath(roundedRect: r, cornerWidth: radius, cornerHeight: radius, transform: nil)
}

// macOS icon grid: 824 pt body centred in 1024, continuous-ish corner radius.
let body = CGRect(x: 100, y: 100, width: 824, height: 824)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: color(0x000000, 0.22))
ctx.addPath(rounded(body, 186)); ctx.setFillColor(color(0xFBFBFC)); ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(rounded(body, 186)); ctx.clip()
// Sidebar strip: frosted grey, like the app's 200 px glass sidebar.
let sidebar = CGRect(x: body.minX, y: body.minY, width: 250, height: body.height)
ctx.setFillColor(color(0xE9ECEF)); ctx.fill(sidebar)
ctx.setFillColor(color(0xD9DDE2)); ctx.fill(CGRect(x: sidebar.maxX - 3, y: body.minY, width: 3, height: body.height))
// Sidebar rows: 28 px rows with 7 px radius, scaled; the selected one brighter.
var y = body.maxY - 190
for i in 0..<5 {
    let row = CGRect(x: sidebar.minX + 44, y: y, width: [150, 120, 160, 104, 136][i], height: 30)
    if i == 1 {
        ctx.addPath(rounded(CGRect(x: sidebar.minX + 26, y: y - 20, width: 198, height: 70), 22))
        ctx.setFillColor(color(0xFFFFFF)); ctx.fillPath()
        ctx.addPath(rounded(row, 15)); ctx.setFillColor(color(0x1D5FA8)); ctx.fillPath()
    } else {
        ctx.addPath(rounded(row, 15)); ctx.setFillColor(color(0xB8BEC6)); ctx.fillPath()
    }
    y -= 104
}
// Reading pane: a heading and body lines in graphite.
let left = sidebar.maxX + 78
ctx.addPath(rounded(CGRect(x: left, y: body.maxY - 206, width: 330, height: 56), 28))
ctx.setFillColor(color(0x24282D)); ctx.fillPath()
var ly = body.maxY - 312
for w in [430, 440, 390, 420, 300] as [CGFloat] {
    ctx.addPath(rounded(CGRect(x: left, y: ly, width: w, height: 30), 15))
    ctx.setFillColor(color(0x8A929B)); ctx.fillPath()
    ly -= 78
}
// Caret after the last line.
ctx.addPath(rounded(CGRect(x: left + 300 + 22, y: ly + 78 - 16, width: 12, height: 62), 6))
ctx.setFillColor(color(0x1D5FA8)); ctx.fillPath()
ctx.restoreGState()

// Hairline edge so the light icon holds up on a light Dock.
ctx.addPath(rounded(body.insetBy(dx: 1, dy: 1), 185)); ctx.setStrokeColor(color(0x000000, 0.08))
ctx.setLineWidth(2); ctx.strokePath()

let dir = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
let iconset = dir.appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
let png = rep.representation(using: .png, properties: [:])!
let master = dir.appendingPathComponent("AppIcon-1024.png")
try! png.write(to: master)
for (name, px) in [("16x16", 16), ("16x16@2x", 32), ("32x32", 32), ("32x32@2x", 64), ("128x128", 128),
                   ("128x128@2x", 256), ("256x256", 256), ("256x256@2x", 512), ("512x512", 512), ("512x512@2x", 1024)] {
    let out = iconset.appendingPathComponent("icon_\(name).png").path
    let p = Process(); p.executableURL = URL(fileURLWithPath: "/usr/bin/sips")
    p.arguments = ["-z", "\(px)", "\(px)", master.path, "--out", out]
    p.standardOutput = FileHandle.nullDevice; try! p.run(); p.waitUntilExit()
}
let u = Process(); u.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
u.arguments = ["-c", "icns", iconset.path, "-o", dir.appendingPathComponent("AppIcon.icns").path]
try! u.run(); u.waitUntilExit()
try? FileManager.default.removeItem(at: iconset)
print("wrote", dir.appendingPathComponent("AppIcon.icns").path)
