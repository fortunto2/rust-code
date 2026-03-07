use std::time::Duration;
use std::thread;

const WIDTH: usize = 40;
const HEIGHT: usize = 20;

fn main() {
    let mut grid = [[false; WIDTH]; HEIGHT];
    let mut seed = 123456789u32;
    
    // Псевдослучайная инициализация поля
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            grid[y][x] = (seed >> 24) % 3 == 0; // ~33% заполненность
        }
    }

    print!("\x1B[2J"); // Очистка экрана один раз
    
    loop {
        print!("\x1B[1;1H"); // Возврат курсора в левый верхний угол (чтобы не мерцало)
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                print!("{}", if grid[y][x] { "██" } else { "  " });
            }
            println!();
        }

        let mut next_grid = [[false; WIDTH]; HEIGHT];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let mut neighbors = 0;
                for dy in [-1, 0, 1].iter() {
                    for dx in [-1, 0, 1].iter() {
                        if *dx == 0 && *dy == 0 { continue; }
                        let ny = (y as isize + dy).rem_euclid(HEIGHT as isize) as usize;
                        let nx = (x as isize + dx).rem_euclid(WIDTH as isize) as usize;
                        if grid[ny][nx] {
                            neighbors += 1;
                        }
                    }
                }
                next_grid[y][x] = match (grid[y][x], neighbors) {
                    (true, 2) | (true, 3) => true,
                    (false, 3) => true,
                    _ => false,
                };
            }
        }
        grid = next_grid;
        thread::sleep(Duration::from_millis(150));
    }
}
