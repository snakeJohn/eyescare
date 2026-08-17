/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        brand: {
          300: "#f0d18a",
          400: "#e4b04a",
          500: "#c9922a",
          600: "#a67418",
        },
        surface: {
          DEFAULT: "var(--bg)",
          card: "var(--card)",
          raised: "var(--raised)",
          border: "var(--line)",
        },
      },
      fontFamily: {
        sans: [
          "Segoe UI Variable",
          "system-ui",
          "PingFang SC",
          "Microsoft YaHei",
          "Segoe UI",
          "sans-serif",
        ],
      },
      boxShadow: {
        inset: "inset 0 1px 0 0 rgb(255 255 255 / 0.04)",
      },
    },
  },
  plugins: [],
};
