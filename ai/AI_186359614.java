import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_186359614 extends AI {
public AI_186359614() throws LeekRunException {
super(4, 2);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_a = ops(1l, 1);
Object u_b = ops(2l, 1);
if (ops(ops(eq(u_a, 1l), 2) && ops(eq(u_b, 2l), 1), 1)) {
return 1l;
}
else {
return 0l;
}
}
protected String getAIString() { return "Main_1a99d41903962_35";}
protected String[] getErrorFiles() { return new String[] {"Main_1a99d41903962_35", };}

protected int[] getErrorFilesID() { return new int[] {186359614, };}

}
