import pathlib, subprocess, sys, tempfile, unittest

class FallbackCleanup(unittest.TestCase):
    def test_controlled_cleanup_success_failure_and_original_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            saved = root/'fixture/.theseus-comparisons/owned/comparison.json'
            saved.parent.mkdir(parents=True); saved.write_text('{}')
            runner = root/'run.py'
            runner.write_text('''import pathlib,sys
sys.path.insert(0, sys.argv[1])
from comparison_cleanup import fallback_cleanup
root=pathlib.Path(sys.argv[2]); original=sys.argv[3]=='original'
try:
    if original: raise AssertionError('PRIMARY ASSERTION')
finally:
    fallback_cleanup(sys.executable, [root/'fixture'], root, sys.exc_info()[1], prefix_args=[str(root/'fake.py')])
''')
            fake = root/'fake.py'
            for status, original in [(0,False),(7,False),(7,True)]:
                fake.write_text(f"import sys\nassert sys.argv[1] == 'cleanup'\nassert sys.argv[2].endswith('/fixture/.theseus-comparisons/owned/comparison.json')\nassert len(sys.argv) == 3\nprint('saved stdout')\nprint('saved stderr',file=sys.stderr)\nsys.exit({status})\n")
                out = subprocess.run([sys.executable,str(runner),str(pathlib.Path(__file__).parent.resolve()),str(root),'original' if original else 'normal'],capture_output=True,text=True)
                if status==0: self.assertEqual(out.returncode,0,out.stderr)
                else:
                    self.assertNotEqual(out.returncode,0)
                    self.assertIn('fallback cleanup failed',out.stderr)
                if original:
                    self.assertIn('PRIMARY ASSERTION',out.stderr)
                    self.assertTrue(out.stderr.rstrip().endswith('PRIMARY ASSERTION'),out.stderr)
            statuses=list(root.glob('fallback-cleanup-*.status'))
            self.assertEqual(len(statuses),3)
            self.assertEqual(sorted(p.read_text() for p in statuses),['0','7','7'])
            for p in statuses:
                self.assertEqual(p.with_suffix('.stdout').read_text(),'saved stdout\n')
                self.assertEqual(p.with_suffix('.stderr').read_text(),'saved stderr\n')
            self.assertTrue(saved.exists())
if __name__=='__main__': unittest.main()
