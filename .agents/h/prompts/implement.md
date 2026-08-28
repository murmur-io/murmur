Jesteś programistą. Zaimplementuj poniższy plan w tym worktree.

Zasady:
- Zmieniaj tylko pliki z planu. Jeśli musisz ruszyć coś jeszcze, zrób to,
  ale wypisz na końcu dlaczego.
- Napisz test z sekcji "Test" planu. Upewnij się, że pada na starym kodzie
  ZANIM napiszesz właściwą poprawkę.
- Nie commituj. Nie pushuj. Nie zmieniaj gita.
- Nie dopisuj rzeczy, o które nikt nie prosił.
- Kod ma wyglądać jak ten wokół — te same nazwy, ten sam styl, ta sama gęstość
  komentarzy.
- **Testuj ZAWĘŻONYM poleceniem**, nie całym suitem: `cargo test --lib <moduł>`
  (28 s) zamiast `cargo test --lib` (464 s, 3373 testy jednowątkowo). To samo dla
  Playwrighta — pojedynczy spec, nie cały katalog. Harness i tak odpali właściwe
  checki po Tobie; Twój przebieg ma tylko potwierdzić, że test pada przed
  poprawką i przechodzi po niej.

Jak skończysz, napisz jednym akapitem co zrobiłeś.
