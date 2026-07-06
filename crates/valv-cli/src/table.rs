use std::fmt::Write as _;

pub(crate) fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    print!("{}", render_table(headers, rows));
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths = headers
        .iter()
        .map(|header| header.chars().count())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index >= widths.len() {
                continue;
            }
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut output = String::new();
    write_row(
        &mut output,
        &headers
            .iter()
            .map(|header| (*header).to_owned())
            .collect::<Vec<_>>(),
        &widths,
    );
    write_row(
        &mut output,
        &widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>(),
        &widths,
    );
    for row in rows {
        write_row(&mut output, row, &widths);
    }
    output
}

fn write_row(output: &mut String, row: &[String], widths: &[usize]) {
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            output.push_str("  ");
        }
        let value = row.get(index).map(String::as_str).unwrap_or("");
        write!(output, "{value:<width$}").expect("writing to String cannot fail");
    }
    output.push('\n');
}

#[cfg(test)]
mod tests {
    use super::render_table;

    #[test]
    fn mixed_ascii_and_multibyte_cells_keep_column_boundaries() {
        let rows = vec![
            vec!["alice".to_owned(), "reader".to_owned()],
            vec!["日本語".to_owned(), "writer".to_owned()],
        ];
        let output = render_table(&["name", "role"], &rows);
        let lines = output.lines().collect::<Vec<_>>();

        let ascii_role_start = char_position(lines[2], "reader");
        let cjk_role_start = char_position(lines[3], "writer");

        assert_eq!(ascii_role_start, cjk_role_start);
    }

    fn char_position(line: &str, needle: &str) -> usize {
        let byte_position = line.find(needle).expect("needle should exist in line");
        line[..byte_position].chars().count()
    }
}
