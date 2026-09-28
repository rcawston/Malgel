# Welcome to Malgel

Malgel is a fast, native Markdown editor. Write on the left and the preview on the right follows along as you type and scroll. Scroll the preview and the editor follows it.

> Everything here is ordinary Markdown. Edit this page to try things out, or press **Ctrl+N** (**⌘N** on macOS) to start a new document.

## Writing

You can use **bold**, _italic_, ~~strikethrough~~, `inline code` and [links](https://commonmark.org/help/). Select some text and press **Ctrl+B** to make it bold, or **Ctrl+K** to turn it into a link.

- Lists nest with two spaces
  - like this
- [x] Task lists are supported
- [ ] Finish the first draft

1. Numbered lists
2. keep their order

## Code

```rust
fn main() {
    let words = ["fast", "native", "focused"];
    println!("Malgel is {}.", words.join(", "));
}
```

Code blocks are highlighted for most popular languages, and each one has a copy button.

## Tables

| Command            | Linux and Windows | macOS  |
| ------------------ | ----------------- | ------ |
| Open               | Ctrl+O            | ⌘O     |
| Save               | Ctrl+S            | ⌘S     |
| Go to heading      | Ctrl+Shift+O      | ⌘⇧O    |
| Editor / Split / Preview | Ctrl+1 / 2 / 3 | ⌘1 / 2 / 3 |
| Export as HTML     | Ctrl+Shift+E      | ⌘⇧E    |

## Everything else

Drop a Markdown file onto the window to open it. Zoom with **Ctrl+=** and **Ctrl+-**, switch between light and dark with **Ctrl+Shift+L**, and pick a theme from the **View** menu.

---

Happy writing.
