#set page(paper: "a4", margin: 1.5cm)
#for p in range(200) [
  #text(size: 14pt)[Diagramm #(p + 1)]
  #box(width: 100%, height: 22cm)[
    #for k in range(600) {
      let x = calc.rem(k * 37 + p * 11, 170)
      let y = calc.rem(k * 53 + p * 7, 210)
      place(dx: x * 1mm, dy: y * 1mm, curve(curve.move((0mm, 0mm)), curve.cubic((5mm, -6mm), (9mm, 6mm), (14mm, 0mm)), stroke: 0.4pt + rgb(calc.rem(k, 255), 60, 120)))
    }
  ]
  #pagebreak(weak: true)
]
