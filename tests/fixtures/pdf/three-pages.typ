// Test fixture (own work): 3 pages, searchable text on page 2.
#set page(paper: "a5", margin: 1.5cm)
#set text(font: "DejaVu Sans", size: 11pt)
= Seite 1
Erste Seite mit einem Kreis. #circle(radius: 1cm)
#pagebreak()
= Seite 2
Das Suchwort ist Zebrafisch. The search word is zebrafish.
#pagebreak()
= Seite 3
#rect(width: 80%, height: 3cm)[Diagramm]
