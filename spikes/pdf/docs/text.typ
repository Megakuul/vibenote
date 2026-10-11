#set page(paper: "a4", margin: 2cm, numbering: "1")
#set text(font: "DejaVu Sans", size: 10pt, lang: "de")
#for i in range(2000) [
  = Kapitel #(i + 1): Vorlesung Analysis
  Gegeben sei die Funktion $f(x) = x^2 + #i x$. Seite #(i + 1). Suchwort-#(i + 1) zebrafisch#(calc.rem(i, 97)).
  #lorem(380)
  $ integral_0^1 f(x) dif x = 1/3 + #i / 2 $
  #lorem(120)
  #pagebreak(weak: true)
]
