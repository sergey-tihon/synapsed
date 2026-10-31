import QtQuick
import QtQuick.Shapes

Shape {
    id: brandGlyph
    property color foregroundColor: "white"
    transform: Scale {
        xScale: Math.min(brandGlyph.width, brandGlyph.height) / 1024
        yScale: Math.min(brandGlyph.width, brandGlyph.height) / 1024
    }

    ShapePath {
        strokeWidth: 68
        capStyle: ShapePath.RoundCap
        joinStyle: ShapePath.RoundJoin
        strokeColor: foregroundColor
        fillColor: "transparent"
        PathSvg { path: "M34 456H112C172 456 204 442 248 402L340 316C390 268 420 232 484 232H540C604 232 634 268 684 316L776 402C820 442 852 456 912 456H990M34 568H112C172 568 204 582 248 622L340 708C390 756 420 792 484 792H540C604 792 634 756 684 708L776 622C820 582 852 568 912 568H990" }
    }
    ShapePath {
        strokeWidth: 56
        capStyle: ShapePath.RoundCap
        joinStyle: ShapePath.RoundJoin
        strokeColor: foregroundColor
        fillColor: "transparent"
        PathSvg { path: "M454 352H438C404 352 392 370 392 402V474C392 496 380 512 352 512C380 512 392 528 392 550V622C392 654 404 672 438 672H454M570 352H586C620 352 632 370 632 402V474C632 496 644 512 672 512C644 512 632 528 632 550V622C632 654 620 672 586 672H570" }
    }
}
