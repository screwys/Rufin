import os
from tqdm import tqdm

NUM_DIRS = 30
RECURSION_DEPTH = 4

def mkdirs(rec: int, p: str) -> None:
    if rec == 0:
        t = tqdm(range(NUM_DIRS), desc="Creating directories")
    else:
        t = range(NUM_DIRS)
    for i in t:
        dir_name = f"dir_{i}"
        # print(f"Creating directory: {dir_name}")
        os.makedirs(p + "\\" + dir_name, exist_ok=True)
        if rec < RECURSION_DEPTH:
            mkdirs(rec + 1, p + "\\" + dir_name)
        else:
            with open(p + "\\" + dir_name + "\\file.txt", "w") as f:
                f.write("Hello, World!")


if __name__ == "__main__":
    mkdirs(0, "C:\\Users")
