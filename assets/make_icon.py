"""Gera o ícone do HueBeat (assets/icon.ico e assets/icon.png).

Execute com:  python assets/make_icon.py
"""

import math
import os

from PIL import Image, ImageDraw, ImageFilter

AQUI = os.path.dirname(os.path.abspath(__file__))
S = 2048                      # resolução de trabalho
MARGEM = int(S * 0.035)
RAIO = int(S * 0.225)

FUNDO_TOPO = (12, 14, 22)
FUNDO_BASE = (24, 28, 44)
CORES = [
    (34, 211, 238),           # ciano
    (56, 189, 248),           # azul
    (129, 140, 248),          # índigo
    (192, 132, 252),          # violeta
    (244, 114, 182),          # rosa
]
ALTURAS = [0.34, 0.60, 0.88, 0.52, 0.74]


def gradiente_vertical(tamanho, topo, base):
    img = Image.new("RGB", (tamanho, tamanho), topo)
    d = ImageDraw.Draw(img)
    for y in range(tamanho):
        t = y / max(1, tamanho - 1)
        d.line(
            [(0, y), (tamanho, y)],
            fill=tuple(int(topo[i] + (base[i] - topo[i]) * t) for i in range(3)),
        )
    return img


def mascara_arredondada(tamanho, margem, raio):
    m = Image.new("L", (tamanho, tamanho), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        [margem, margem, tamanho - margem - 1, tamanho - margem - 1],
        radius=raio,
        fill=255,
    )
    return m


def barra(draw, x, largura, altura, cor):
    topo = int(S * 0.50 - altura / 2)
    base = int(S * 0.50 + altura / 2)
    draw.rounded_rectangle(
        [x, topo, x + largura, base],
        radius=largura // 2,
        fill=cor,
    )


def main():
    base = gradiente_vertical(S, FUNDO_TOPO, FUNDO_BASE)

    # brilho suave no canto superior esquerdo
    brilho = Image.new("L", (S, S), 0)
    ImageDraw.Draw(brilho).ellipse(
        [-S * 0.25, -S * 0.35, S * 0.75, S * 0.65], fill=90
    )
    brilho = brilho.filter(ImageFilter.GaussianBlur(S * 0.12))
    azul = Image.new("RGB", (S, S), (34, 211, 238))
    base = Image.composite(azul, base, brilho.point(lambda v: int(v * 0.35)))

    # barras do equalizador
    largura = int(S * 0.082)
    vao = int(S * 0.052)
    total = len(CORES) * largura + (len(CORES) - 1) * vao
    x = (S - total) // 2

    camada = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(camada)
    for i, cor in enumerate(CORES):
        barra(d, x + i * (largura + vao), largura, ALTURAS[i] * S * 0.62, cor + (255,))

    # halo colorido atrás das barras
    halo = camada.filter(ImageFilter.GaussianBlur(S * 0.035))
    halo.putalpha(halo.getchannel("A").point(lambda v: int(v * 0.75)))
    base = Image.alpha_composite(base.convert("RGBA"), halo)

    # barras nítidas com leve gradiente (topo mais claro)
    nitida = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    dn = ImageDraw.Draw(nitida)
    for i, cor in enumerate(CORES):
        bx = x + i * (largura + vao)
        h = ALTURAS[i] * S * 0.62
        topo = int(S * 0.50 - h / 2)
        base_y = int(S * 0.50 + h / 2)
        for y in range(topo, base_y):
            t = (y - topo) / max(1, base_y - topo)
            c = tuple(int(cor[j] + (255 - cor[j]) * (1 - t) * 0.35) for j in range(3))
            dn.line([(bx, y), (bx + largura, y)], fill=c + (255,))
        dn.rounded_rectangle(
            [bx, topo, bx + largura, base_y], radius=largura // 2, outline=c + (255,), width=0
        )
    base = Image.alpha_composite(base, nitida)

    # moldura interna sutil
    moldura = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(moldura).rounded_rectangle(
        [MARGEM, MARGEM, S - MARGEM - 1, S - MARGEM - 1],
        radius=RAIO,
        outline=(255, 255, 255, 26),
        width=int(S * 0.006),
    )
    base = Image.alpha_composite(base, moldura)

    # recorte arredondado
    mask = mascara_arredondada(S, MARGEM, RAIO)
    final = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    final.paste(base, (0, 0), mask)

    # reduz com qualidade e salva
    grandes = final.resize((512, 512), Image.LANCZOS)
    grandes.save(os.path.join(AQUI, "icon.png"))

    tamanhos = [16, 24, 32, 48, 64, 128, 256]
    grandes.save(
        os.path.join(AQUI, "icon.ico"),
        format="ICO",
        sizes=[(t, t) for t in tamanhos],
    )
    print("Ícones gerados em", AQUI)


if __name__ == "__main__":
    main()
