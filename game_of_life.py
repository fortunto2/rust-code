import time
import random
import os

def get_neighbors(grid, y, x, rows, cols):
    count = 0
    for i in range(-1, 2):
        for j in range(-1, 2):
            if i == 0 and j == 0: continue
            r, c = y + i, x + j
            if 0 <= r < rows and 0 <= c < cols:
                count += grid[r][c]
    return count

def update(grid, rows, cols):
    new_grid = [[0 for _ in range(cols)] for _ in range(rows)]
    for y in range(rows):
        for x in range(cols):
            neighbors = get_neighbors(grid, y, x, rows, cols)
            if grid[y][x] == 1:
                new_grid[y][x] = 1 if neighbors in [2, 3] else 0
            else:
                new_grid[y][x] = 1 if neighbors == 3 else 0
    return new_grid

def main():
    rows, cols = 20, 40
    grid = [[random.choice([0, 1]) for _ in range(cols)] for _ in range(rows)]

    try:
        while True:
            # Очистка экрана (ANSI sequence)
            os.system('cls' if os.name == 'nt' else 'clear')
            
            output = []
            for y in range(rows):
                row_str = ''.join(['█' if grid[y][x] else ' ' for x in range(cols)])
                output.append(row_str)
            print('\n'.join(output))
            
            grid = update(grid, rows, cols)
            time.sleep(0.2)
    except KeyboardInterrupt:
        print("\nИгра остановлена.")

if __name__ == '__main__':
    main()