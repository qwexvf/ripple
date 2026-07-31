export function main() {
  return "not an element";
}

export function Widget() {
  return <span>hi</span>;
}

export function Page() {
  return (
    <main>
      <Widget />
    </main>
  );
}
